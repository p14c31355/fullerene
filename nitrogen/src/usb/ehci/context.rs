//! EHCI Context — unified container for all EHCI state.
//!
//! # Design
//!
//! ```text
//! EhciContext
//! ├── EhciRegisterContext  (MMIO, CAPLENGTH, Operational)
//! ├── TransferContext      (AsyncSchedule, QueueHeadPool, QtdPool)
//! ├── EhciPortContext      (port state tracking)
//! └── devices: Vec<UsbDevice>
//! ```
//!
//! # Usage
//!
//! ```ignore
//! let mut ehci = EhciContext::new(mmio_base, driver_ctx)?;
//! ehci.reset()?;
//! ehci.start();
//! ehci.poll_ports();
//! ```

use alloc::vec::Vec;
use core::ptr;

use crate::DriverContext;
use crate::pci_health::PciHealth;
use crate::usb::{UsbDevice, UsbDirection, UsbSetupPacket, UsbSpeed};

// ── Import sub-contexts from sibling modules ──────────────────
use super::async_queue::*;
use super::port::*;
use super::register::*;
use crate::usb::host_controller::HostController;

// ============================================================================
//  EhciContext — top-level EHCI state container
// ============================================================================

/// Unified EHCI host controller state.
pub struct EhciContext {
    /// MMIO register access.
    pub registers: EhciRegisterContext,
    /// Async schedule + qH/qTD pools.
    pub transfer: TransferContext,
    /// Port management.
    pub ports: EhciPortContext,
    /// Discovered USB devices.
    pub devices: Vec<UsbDevice>,
    /// Driver context for memory allocation.
    driver_ctx: &'static dyn DriverContext,
    /// PCI health monitor — check before MMIO transaction cycles.
    pub health: PciHealth,
    /// Next USB device address to assign.
    pub next_address: u8,
}

// ============================================================================
//  HostController trait impl
// ============================================================================

impl HostController for EhciContext {
    fn reset(&mut self) -> Result<(), &'static str> {
        self.reset()
    }

    fn start(&mut self) -> Result<(), &'static str> {
        self.start()
    }

    fn poll_ports(&mut self) -> usize {
        self.poll_ports()
    }

    fn clear_devices(&mut self) {
        self.clear_devices();
    }

    fn n_ports(&self) -> u32 {
        self.n_ports()
    }

    fn devices(&self) -> &[UsbDevice] {
        self.devices()
    }

    fn devices_mut(&mut self) -> &mut [UsbDevice] {
        self.devices_mut()
    }

    fn control_transfer(
        &mut self,
        dev_addr: u8,
        setup: &UsbSetupPacket,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        // Control transfers always go to endpoint 0
        self.control_transfer(dev_addr, 0, setup, buf)
    }

    fn bulk_transfer(
        &mut self,
        dev_addr: u8,
        endpoint: u8,
        buf: &mut [u8],
        dir: UsbDirection,
        mps: u16,
    ) -> Result<usize, &'static str> {
        self.bulk_transfer(dev_addr, endpoint, buf, dir, mps)
    }
}

// SAFETY: EHCI is used only on the main kernel thread (single-threaded kernel).
unsafe impl Send for EhciContext {}

impl EhciContext {
    /// Create a new EHCI context from the MMIO base address.
    /// # Safety
    /// `mmio_base` must reference a mapped EHCI register BAR for the lifetime
    /// of the returned controller.
    pub unsafe fn new(mmio_base: *mut u8, ctx: &'static dyn DriverContext, health: PciHealth) -> Option<Self> {
        let registers = unsafe { EhciRegisterContext::new(mmio_base) };
        let hcsparams = unsafe { ptr::read_volatile(mmio_base.add(4) as *const u32) };
        let n_ports = (hcsparams & 0x0F).max(1);

        let transfer = TransferContext::alloc(ctx)?;
        let ports = EhciPortContext::new(n_ports);

        Some(Self {
            registers,
            transfer,
            ports,
            devices: Vec::new(),
            driver_ctx: ctx,
            health,
            next_address: 1,
        })
    }

    /// Get a reference to the driver context.
    pub fn driver_ctx(&self) -> &dyn DriverContext {
        self.driver_ctx
    }

    // ── Initialisation ─────────────────────────────────────────

    /// Reset the controller (HCRESET).
    pub fn reset(&mut self) -> Result<(), &'static str> {
        if !self.health.is_device_present() {
            log::error!("EHCI: device gone before reset");
            return Err("EHCI device gone");
        }
        let op = &self.registers.op;
        op.set_usbcmd(USBCMD_HCRESET);
        if crate::timing::wait_timeout_us(500_000, || {
            op.usbcmd() & USBCMD_HCRESET == 0
        }).is_err() {
            return Err("HCRESET timeout");
        }
        Ok(())
    }

    /// Start the controller and enable the async schedule.
    pub fn start(&mut self) -> Result<(), &'static str> {
        let op = &self.registers.op;
        op.set_async_list_addr(self.transfer.schedule.head_phys as u32);

        let cmd = op.usbcmd();
        op.set_usbcmd(cmd | USBCMD_RS | USBCMD_ASSE);

        // Wait for HCHalted to clear
        if crate::timing::wait_timeout_us(200_000, || {
            op.usbsts() & USBSTS_HCH == 0
        }).is_err() {
            return Err("EHCI start timeout (HCH still set)");
        }

        // Clear stale port-change status bits
        op.write_usbsts(USBSTS_PCD);
        Ok(())
    }

    /// Check if the controller is running.
    pub fn is_running(&self) -> bool {
        self.registers.op.usbsts() & USBSTS_HCH == 0
    }

    // ── Port polling ───────────────────────────────────────────

    /// Poll all ports for newly connected devices.
    ///
    /// Uses PCD (Port Change Detect) to re-evaluate ports on hotplug.
    /// Returns the number of newly discovered devices.
    pub fn poll_ports(&mut self) -> usize {
        let op = &self.registers.op;
        let initial_count = self.devices.len();
        let sts = op.usbsts();
        let pcd = sts & USBSTS_PCD != 0;

        // Clear PCD so we get fresh port changes next time
        op.write_usbsts(USBSTS_PCD);

        for port_idx in 0..self.ports.n_ports {
            let portsc = op.portsc(port_idx);
            let has_dev = portsc & PORTSC_CCS != 0;

            // PCD → re-evaluate this port
            if pcd && !has_dev {
                self.ports.processed_mask &= !(1 << port_idx);
                continue;
            }
            if pcd && has_dev && self.ports.is_processed(port_idx) {
                self.ports.processed_mask &= !(1 << port_idx);
            }

            // Already processed → skip
            if self.ports.is_processed(port_idx) {
                continue;
            }
            // No device → leave unmarked, will poll again
            if !has_dev {
                continue;
            }

            // Port reset (EHCI spec §4.2.4: PR must be cleared by HC, not by driver)
            op.write_portsc(port_idx, portsc | PORTSC_RESET);
            let pr_cleared = crate::timing::wait_timeout_us(200_000, || {
                op.portsc(port_idx) & PORTSC_RESET == 0
            }).is_ok();
            if !pr_cleared {
                self.ports.mark_processed(port_idx);
                continue;
            }

            // Wait for PE
            crate::timing::wait_timeout_us(10_000, || {
                op.portsc(port_idx) & PORTSC_PE != 0
            }).ok();

            // Check CCS survived
            if op.portsc(port_idx) & PORTSC_CCS == 0 {
                self.ports.mark_processed(port_idx);
                continue;
            }

            let speed = UsbSpeed::from_portsc(op.portsc(port_idx));
            if speed != UsbSpeed::High {
                // EHCI only handles High-speed devices directly
                self.ports.mark_processed(port_idx);
                continue;
            }

            self.devices.push(UsbDevice {
                address: 0,
                speed,
                max_packet_size_0: 64,
                vendor_id: 0,
                product_id: 0,
                device_class: 0,
                device_subclass: 0,
                device_protocol: 0,
                configurations: 0,
                endpoints: Vec::new(),
                port_index: port_idx,
            });
            self.ports.mark_processed(port_idx);
        }

        self.devices.len() - initial_count
    }

    // ── Device accessors ───────────────────────────────────────

    pub fn devices(&self) -> &[UsbDevice] {
        &self.devices
    }

    pub fn devices_mut(&mut self) -> &mut [UsbDevice] {
        &mut self.devices
    }

    pub fn n_ports(&self) -> u32 {
        self.ports.n_ports
    }

    pub fn read_portsc(&self, port: u32) -> u32 {
        self.registers.op.portsc(port)
    }

    /// Compatibility alias — use [`read_portsc`] instead.
    pub fn portsc(&self, port: u32) -> u32 {
        self.read_portsc(port)
    }

    /// Clear the device list and reset port-done flags.
    pub fn clear_devices(&mut self) {
        self.devices.clear();
        self.ports.clear_processed();
    }

    pub fn reset_pools(&mut self) {
        self.transfer.reset_pools();
    }

    /// Look up a device's speed by its USB address. Falls back to High speed
    /// if the device is not found (EHCI only handles High speed natively).
    fn device_speed(&self, dev_addr: u8) -> UsbSpeed {
        self.devices
            .iter()
            .find(|d| d.address == dev_addr)
            .map(|d| d.speed)
            .unwrap_or(UsbSpeed::High)
    }

    /// Wait for an Async Advance interrupt (AAINT) after unlinking a qH.
    ///
    /// After removing a qH from the async schedule, the controller may still
    /// have it cached.  Issuing IAAD (Interrupt on Async Advance Doorbell)
    /// and waiting for AAINT ensures the controller has flushed its cache
    /// and will no longer access the freed qH, qTDs, or staging buffers.
    /// Returns an error if AAINT does not arrive within the timeout.
    fn wait_async_advance(&self, op: &EhciOperationalRegisters) -> Result<(), &'static str> {
        // Clear any stale AAINT before ringing IAAD
        op.write_usbsts(USBSTS_AAINT);
        op.set_usbcmd_bits(USBCMD_IAAD);
        if crate::timing::wait_timeout_us(1_000_000, || {
            let sts = op.usbsts();
            let ready = sts & USBSTS_AAINT != 0;
            if ready {
                op.write_usbsts(USBSTS_AAINT);
            }
            ready
        }).is_err() {
            return Err("async advance timeout");
        }
        Ok(())
    }

    // ── Control transfer ───────────────────────────────────────

    /// Perform a USB control transfer.
    ///
    /// Creates a SETUP → DATA → STATUS qTD chain, inserts into the async
    /// schedule, waits for completion, and frees resources.
    pub fn control_transfer(
        &mut self,
        dev_addr: u8,
        endpoint: u8,
        setup: &UsbSetupPacket,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        let is_in = (setup.bm_request_type & 0x80) != 0;
        let data_len = setup.w_length as usize;
        if data_len > 4096 {
            return Err("control transfer data phase too large (> 4096 bytes)");
        }

        // Allocate qH
        let (qh, qh_phys) = self.transfer.qh_pool.allocate().ok_or("no qH")?;
        let speed = self.device_speed(dev_addr);

        unsafe {
            ptr::write_volatile(&mut qh.ep_chars, qh_ep_chars(dev_addr, endpoint, speed, 64));
            ptr::write_volatile(&mut qh.ep_caps, 0);
            ptr::write_volatile(&mut qh.current_qtd, QTD_TERMINATE);
        }

        // Allocate qTDs (with cleanup on failure)
        let (qtd_setup, qtd_setup_phys) = match self.transfer.qtd_pool.allocate() {
            Some(val) => val,
            None => {
                self.transfer.qh_pool.free(qh);
                return Err("no setup qTD");
            }
        };

        let has_data = data_len > 0;
        let mut qtd_data: Option<(&mut Qtd, u64)> = if has_data {
            match self.transfer.qtd_pool.allocate() {
                Some(val) => Some(val),
                None => {
                    self.transfer.qtd_pool.free(qtd_setup);
                    self.transfer.qh_pool.free(qh);
                    return Err("no data qTD");
                }
            }
        } else {
            None
        };

        let (qtd_status, qtd_status_phys) = match self.transfer.qtd_pool.allocate() {
            Some(val) => val,
            None => {
                self.transfer.qtd_pool.free(qtd_setup);
                if let Some((d, _)) = qtd_data {
                    self.transfer.qtd_pool.free(d);
                }
                self.transfer.qh_pool.free(qh);
                return Err("no status qTD");
            }
        };

        // Allocate staging buffer for SETUP (8 bytes)
        let setup_page_phys = match self.driver_ctx.allocate_contiguous_frames(1) {
            Ok(phys) => phys,
            Err(_) => {
                self.transfer.qtd_pool.free(qtd_setup);
                if let Some((d, _)) = qtd_data {
                    self.transfer.qtd_pool.free(d);
                }
                self.transfer.qtd_pool.free(qtd_status);
                self.transfer.qh_pool.free(qh);
                return Err("no setup staging memory");
            }
        };
        let setup_page_virt = self.driver_ctx.phys_to_virt(setup_page_phys) as *mut u8;
        let setup_bytes =
            unsafe { core::slice::from_raw_parts(setup as *const UsbSetupPacket as *const u8, 8) };
        unsafe {
            ptr::copy_nonoverlapping(setup_bytes.as_ptr(), setup_page_virt, 8);
        }

        // Allocate staging buffer for DATA phase (if needed)
        let (data_staging_phys, data_staging_pages) = if has_data {
            let pages = (data_len + 4095) / 4096;
            match self.driver_ctx.allocate_contiguous_frames(pages) {
                Ok(phys) => (phys, pages),
                Err(_) => {
                    self.driver_ctx.free_contiguous_frames(setup_page_phys, 1);
                    self.transfer.qtd_pool.free(qtd_setup);
                    if let Some((d, _)) = qtd_data {
                        self.transfer.qtd_pool.free(d);
                    }
                    self.transfer.qtd_pool.free(qtd_status);
                    self.transfer.qh_pool.free(qh);
                    return Err("no data staging memory");
                }
            }
        } else {
            (0, 0)
        };
        let data_staging_virt = if data_len > 0 {
            self.driver_ctx.phys_to_virt(data_staging_phys) as *mut u8
        } else {
            core::ptr::null_mut()
        };

        // Build SETUP qTD
        let next_after_setup = qtd_data
            .as_ref()
            .map(|d| d.1 as u32)
            .unwrap_or(qtd_status_phys as u32);
        unsafe {
            ptr::write_volatile(&mut qtd_setup.next_qtd, next_after_setup);
            ptr::write_volatile(&mut qtd_setup.alt_next_qtd, QTD_TERMINATE);
            ptr::write_volatile(
                &mut qtd_setup.token,
                QTD_ACTIVE | QTD_PID_SETUP | QTD_CERR | qtd_total_bytes(8),
            );
            ptr::write_volatile(&mut qtd_setup.buf0, setup_page_phys as u32);
        }

        // Build DATA qTD (if any)
        if data_len > 0 {
            let (data_qh_ptr, _) = qtd_data.as_mut().unwrap();
            if !is_in {
                unsafe {
                    ptr::copy_nonoverlapping(buf.as_ptr(), data_staging_virt, data_len);
                }
            }
            let pid = if is_in { QTD_PID_IN } else { QTD_PID_OUT };
            unsafe {
                ptr::write_volatile(&mut data_qh_ptr.next_qtd, qtd_status_phys as u32);
                ptr::write_volatile(&mut data_qh_ptr.alt_next_qtd, QTD_TERMINATE);
                ptr::write_volatile(
                    &mut data_qh_ptr.token,
                    QTD_ACTIVE | pid | QTD_CERR | qtd_total_bytes(data_len as u32),
                );
                ptr::write_volatile(&mut data_qh_ptr.buf0, data_staging_phys as u32);
            }
        }

        // Build STATUS qTD
        let status_pid = if is_in || data_len == 0 {
            QTD_PID_OUT
        } else {
            QTD_PID_IN
        };
        unsafe {
            ptr::write_volatile(&mut qtd_status.next_qtd, QTD_TERMINATE);
            ptr::write_volatile(&mut qtd_status.alt_next_qtd, QTD_TERMINATE);
            ptr::write_volatile(
                &mut qtd_status.token,
                QTD_ACTIVE | status_pid | QTD_CERR | qtd_total_bytes(0),
            );
        }

        // Link qH → qTD_SETUP
        unsafe {
            ptr::write_volatile(&mut qh.next_qtd, qtd_setup_phys as u32);
            ptr::write_volatile(&mut qh.alt_next_qtd, QTD_TERMINATE);
            ptr::write_volatile(&mut qh.token, QTD_ACTIVE | QTD_PID_SETUP | QTD_CERR);
        }

        // Insert into async schedule
        self.transfer.schedule.insert(qh_phys, self.driver_ctx);

        // Wait for completion
        let timeout = 5_000_000u32;
        let mut result: Result<usize, &'static str> = Ok(0);

        let r = self.transfer.wait_qtd(&qtd_setup, timeout);
        if r.is_err() {
            result = r.map(|_| 0);
        } else if data_len > 0 {
            let (data_qh_ptr, _) = qtd_data.as_ref().unwrap();
            let r2 = self.transfer.wait_qtd(data_qh_ptr, timeout);
            if r2.is_err() {
                result = r2.map(|_| 0);
            } else {
                if is_in {
                    unsafe {
                        ptr::copy_nonoverlapping(data_staging_virt, buf.as_mut_ptr(), data_len);
                    }
                }
                let r3 = self.transfer.wait_qtd(&qtd_status, timeout);
                if r3.is_err() {
                    result = r3.map(|_| 0);
                }
            }
        } else {
            let r3 = self.transfer.wait_qtd(&qtd_status, timeout);
            if r3.is_err() {
                result = r3.map(|_| 0);
            }
        }
        if result.is_ok() {
            result = Ok(data_len);
        }

        // Free resources
        self.transfer.schedule.remove(qh_phys, self.driver_ctx);
        if self.wait_async_advance(&self.registers.op).is_err() {
            log::warn!("EHCI: async advance timeout during control transfer cleanup");
        }
        self.transfer.qtd_pool.free(qtd_setup);
        if let Some((d, _)) = qtd_data {
            self.transfer.qtd_pool.free(d);
        }
        self.transfer.qtd_pool.free(qtd_status);
        self.transfer.qh_pool.free(qh);

        // Free staging buffers
        self.driver_ctx.free_contiguous_frames(setup_page_phys, 1);
        if data_staging_pages > 0 {
            self.driver_ctx
                .free_contiguous_frames(data_staging_phys, data_staging_pages);
        }

        result
    }

    // ── Bulk transfer ──────────────────────────────────────────

    /// Perform a USB bulk transfer (single qTD, up to 20480 bytes).
    pub fn bulk_transfer(
        &mut self,
        dev_addr: u8,
        endpoint: u8,
        buf: &mut [u8],
        dir: UsbDirection,
        max_packet: u16,
    ) -> Result<usize, &'static str> {
        let len = buf.len().min(20480);

        // Allocate qH
        let (qh, qh_phys) = self.transfer.qh_pool.allocate().ok_or("no qH")?;
        let speed = self.device_speed(dev_addr);
        unsafe {
            ptr::write_volatile(
                &mut qh.ep_chars,
                qh_ep_chars(dev_addr, endpoint & 0x0F, speed, max_packet),
            );
            ptr::write_volatile(&mut qh.ep_caps, 0);
            ptr::write_volatile(&mut qh.current_qtd, QTD_TERMINATE);
        }

        // Allocate qTD
        let (qtd, qtd_phys) = match self.transfer.qtd_pool.allocate() {
            Some(val) => val,
            None => {
                self.transfer.qh_pool.free(qh);
                return Err("no qTD");
            }
        };

        // Allocate staging buffer
        let staging_pages = (len + 4095) / 4096;
        let staging_phys = match self.driver_ctx.allocate_contiguous_frames(staging_pages) {
            Ok(phys) => phys,
            Err(_) => {
                self.transfer.qtd_pool.free(qtd);
                self.transfer.qh_pool.free(qh);
                return Err("no staging memory");
            }
        };
        let staging_virt = self.driver_ctx.phys_to_virt(staging_phys) as *mut u8;

        if dir == UsbDirection::Out {
            unsafe {
                ptr::copy_nonoverlapping(buf.as_ptr(), staging_virt, len);
            }
        }

        let pid = match dir {
            UsbDirection::In => QTD_PID_IN,
            UsbDirection::Out => QTD_PID_OUT,
        };

        unsafe {
            ptr::write_volatile(&mut qtd.next_qtd, QTD_TERMINATE);
            ptr::write_volatile(&mut qtd.alt_next_qtd, QTD_TERMINATE);
            ptr::write_volatile(
                &mut qtd.token,
                QTD_ACTIVE | pid | QTD_CERR | qtd_total_bytes(len as u32),
            );
            ptr::write_volatile(&mut qtd.buf0, staging_phys as u32);
            // Populate subsequent buffer pointers for multi-page transfers (up to 20KB)
            let mut p = (staging_phys & !0xFFF) + 0x1000;
            if len > 4096 {
                ptr::write_volatile(&mut qtd.buf1, p as u32);
                p += 0x1000;
            }
            if len > 8192 {
                ptr::write_volatile(&mut qtd.buf2, p as u32);
                p += 0x1000;
            }
            if len > 12288 {
                ptr::write_volatile(&mut qtd.buf3, p as u32);
                p += 0x1000;
            }
            if len > 16384 {
                ptr::write_volatile(&mut qtd.buf4, p as u32);
            }
        }

        unsafe {
            ptr::write_volatile(&mut qh.next_qtd, qtd_phys as u32);
            ptr::write_volatile(&mut qh.alt_next_qtd, QTD_TERMINATE);
            ptr::write_volatile(&mut qh.token, QTD_ACTIVE | pid | QTD_CERR);
        }

        self.transfer.schedule.insert(qh_phys, self.driver_ctx);
        let r = self.transfer.wait_qtd(qtd, 5_000_000);
        if r.is_err() {
            self.transfer.schedule.remove(qh_phys, self.driver_ctx);
            if self.wait_async_advance(&self.registers.op).is_err() {
                log::warn!("EHCI: async advance timeout during bulk transfer cleanup");
            }
            self.driver_ctx
                .free_contiguous_frames(staging_phys, staging_pages);
            self.transfer.qtd_pool.free(qtd);
            self.transfer.qh_pool.free(qh);
            return r.map(|_| 0);
        }

        if dir == UsbDirection::In {
            unsafe {
                ptr::copy_nonoverlapping(staging_virt, buf.as_mut_ptr(), len);
            }
        }

        self.transfer.schedule.remove(qh_phys, self.driver_ctx);
        if self.wait_async_advance(&self.registers.op).is_err() {
            log::warn!("EHCI: async advance timeout during bulk transfer cleanup");
        }
        self.transfer.qtd_pool.free(qtd);
        self.transfer.qh_pool.free(qh);
        self.driver_ctx
            .free_contiguous_frames(staging_phys, staging_pages);
        Ok(len)
    }

}
