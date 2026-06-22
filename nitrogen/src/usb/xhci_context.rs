//! xHCI Context — unified container for all xHCI state.
//!
//! This is the top-level struct that ties together all sub-contexts:
//! registers, rings, devices, ports, and interrupts.
//!
//! # Design
//!
//! ```text
//! XhciContext
//! ├── RegisterContext   (MMIO, Capability, Operational, Runtime, Doorbell)
//! ├── RingContext       (Command Ring, Event Ring)
//! ├── DeviceContextSet  (DCBAA, Scratchpad, SlotManager)
//! ├── PortContext       (port state, reset, link training)
//! └── InterruptContext  (interrupter config, event processing)
//! ```
//!
//! # Usage
//!
//! ```ignore
//! let mut xhci = XhciContext::new(mmio_base, driver_ctx)?;
//! xhci.init()?;
//! xhci.poll_ports();
//! ```

use crate::DriverContext;
use crate::usb::{UsbDevice, UsbDirection, UsbSetupPacket};

use alloc::vec::Vec;
use core::ptr;

// ── Import sub-contexts from sibling modules ──────────────────
use super::host_controller::HostController;
use super::xhci_device::*;
use super::xhci_interrupt::*;
use super::xhci_port::*;
use super::xhci_register::*;
use super::xhci_ring::*;

// ============================================================================
//  XhciContext — top-level xHCI state container
// ============================================================================

/// Unified xHCI host controller state.
///
/// All xHCI state is owned by this struct.  Sub-contexts provide
/// focused access to registers, rings, devices, ports, and interrupts.
pub struct XhciContext {
    /// MMIO register access.
    pub registers: RegisterContext,
    /// Command and event rings.
    pub rings: RingContext,
    /// Device context (DCBAA, slots, scratchpad).
    pub device: DeviceContextSet,
    /// Port management.
    pub ports: PortContext,
    /// Interrupt configuration.
    pub interrupts: InterruptContext,
    /// Discovered USB devices.
    pub devices: Vec<UsbDevice>,
    /// Driver context for memory allocation.
    driver_ctx: &'static dyn DriverContext,
    /// Whether legacy (BIOS→OS) handoff succeeded.
    pub legacy_handoff_done: bool,
}

// SAFETY: xHCI is used only on the main kernel thread (single-threaded kernel).
unsafe impl Send for XhciContext {}

impl XhciContext {
    /// Create a new xHCI context from the MMIO base address.
    ///
    /// This reads capability registers, performs legacy handoff,
    /// and allocates all required data structures (rings, DCBAA, ports).
    pub fn new(mmio_base: *mut u8, ctx: &'static dyn DriverContext) -> Option<Self> {
        // ── Step 1: Read capability registers ─────────────────
        let cap_regs = unsafe { CapabilityRegisters::read(mmio_base) };
        let caplength = cap_regs.caplength;
        let op_off = caplength;
        let rt_off = cap_regs.rt_offset;
        let db_off = cap_regs.db_offset;

        let hcs1 = cap_regs.hcs_params1();
        let hcc1 = cap_regs.hcc_params1();

        let n_ports = hcs1.n_ports;
        let max_slots = hcs1.max_slots;
        let ppc = hcs1.ppc;
        let scratchpad_bufs = hcs1.max_scratchpad_bufs;

        log::info!(
            "xHCI: HCSPARAMS1=0x{:08X} n_ports={} max_slots={} ppc={}",
            cap_regs.hcs_params1,
            n_ports,
            max_slots,
            ppc
        );
        log::info!(
            "xHCI: HCCPARAMS1=0x{:08X} 64bit={} xECP=0x{:x}",
            cap_regs.hcc_params1,
            hcc1.ac64,
            hcc1.ext_cap_ptr
        );

        // ── Step 2: Legacy handoff ────────────────────────────
        let legacy_ok = match try_legacy_handoff(mmio_base, hcc1.ext_cap_ptr) {
            Ok(true) => true,  // OS already owns
            Ok(false) => true, // handoff succeeded
            Err(_) => {
                log::info!("xHCI: legacy handoff failed");
                return None;
            }
        };

        // ── Step 3: Create sub-contexts ───────────────────────
        let op_base = unsafe { mmio_base.add(op_off as usize) };
        let rt_base = unsafe { mmio_base.add(rt_off as usize) };
        let db_base = unsafe { mmio_base.add(db_off as usize) };

        let registers = RegisterContext {
            mmio_base,
            cap: cap_regs,
            op: unsafe { OperationalRegisters::new(op_base) },
            runtime: unsafe { RuntimeRegisters::new(rt_base) },
            doorbell: unsafe { DoorbellRegisters::new(db_base) },
        };

        let rings = RingContext::alloc(ctx, 256, 256)?;
        let device = DeviceContextSet::new(ctx, max_slots, scratchpad_bufs)?;
        let ports = PortContext::new(n_ports, ppc);
        let interrupts = InterruptContext::new();

        Some(Self {
            registers,
            rings,
            device,
            ports,
            interrupts,
            devices: Vec::new(),
            driver_ctx: ctx,
            legacy_handoff_done: legacy_ok,
        })
    }

    /// Get a reference to the driver context.
    pub fn driver_ctx(&self) -> &dyn DriverContext {
        self.driver_ctx
    }

    // ── Register access shortcuts ─────────────────────────────

    /// Read an operational register.
    pub fn op_read(&self, offset: usize) -> u32 {
        self.registers.op.read(offset)
    }

    /// Write an operational register.
    pub fn op_write(&self, offset: usize, val: u32) {
        self.registers.op.write(offset, val);
    }

    /// Ring a doorbell.
    pub fn doorbell(&self, slot: u32, stream: u32) {
        self.registers.doorbell.ring(slot, stream);
    }

    /// Read USBSTS register.
    pub fn usbsts(&self) -> u32 {
        self.registers.op.usbsts().0
    }

    /// Check if the controller is running (HCHalted = 0).
    pub fn is_running(&self) -> bool {
        !self.registers.op.usbsts().hchalted()
    }

    // ── Initialisation ─────────────────────────────────────────

    /// Initialise the controller: reset, configure registers, start.
    pub fn init(&mut self) -> Result<(), &'static str> {
        self.controller_reset()?;
        self.configure_registers()?;
        self.start_controller()?;
        self.clear_hse_and_recover();
        Ok(())
    }

    /// Reset the controller (HCRST) — public for `HostController` trait.
    pub fn reset(&mut self) -> Result<(), &'static str> {
        self.controller_reset()
    }

    /// Internal HCRST logic.
    fn controller_reset(&mut self) -> Result<(), &'static str> {
        let op = &self.registers.op;

        let usbcmd = op.usbcmd();
        let sts = op.usbsts();
        let already_running = !sts.hchalted();
        log::info!(
            "xHCI: USBCMD=0x{:08X} USBSTS=0x{:08X} HCHalted={} already_running={}",
            usbcmd.0,
            sts.0,
            sts.hchalted(),
            already_running
        );

        log::info!("xHCI: performing HCRST");

        // Assert HCRST
        op.set_usbcmd(USBCMD_HCRST);
        for _ in 0..200_000 {
            if !op.usbcmd().reset() {
                break;
            }
        }

        // Wait for HCHalted
        for _ in 0..200_000 {
            if op.usbsts().hchalted() {
                break;
            }
        }

        let sts_after = op.usbsts();
        log::info!(
            "xHCI: after HCRST, USBSTS=0x{:08X} HCHalted={}",
            sts_after.0,
            sts_after.hchalted()
        );

        Ok(())
    }

    /// Configure DCBAA, CRCR, CONFIG, ERST, and interrupter registers.
    fn configure_registers(&mut self) -> Result<(), &'static str> {
        let op = &self.registers.op;
        let rt = &self.registers.runtime;

        // Set DCBAA
        op.set_dcbaap(self.device.dcbaa.phys);

        // Set CRCR (Command Ring Control)
        let crcr_val = self.rings.command.phys() | 1; // RCS=1
        op.set_crcr(crcr_val);

        // Configure max slots
        op.set_config(self.device.slots.max_slots);

        // Allocate ERST page
        let ctx = self.driver_ctx;
        let erst_phys = ctx
            .allocate_contiguous_frames(1)
            .map_err(|_| "no ERST page")?;
        let erst_virt = ctx.phys_to_virt(erst_phys) as *mut ErstEntry;
        unsafe {
            ptr::write_volatile(erst_virt, ErstEntry::new(self.rings.event.phys, 256));
        }

        // Set ERST in runtime registers
        rt.set_erstsz(1);
        rt.set_erstba(erst_phys);

        // Set initial ERDP
        rt.set_erdp(self.rings.event.dequeue_ptr());

        Ok(())
    }

    /// Start the controller — public for `HostController` trait.
    pub fn start(&mut self) -> Result<(), &'static str> {
        self.start_controller()
    }

    /// Internal RS=1 logic.
    fn start_controller(&mut self) -> Result<(), &'static str> {
        let op = &self.registers.op;

        op.set_usbcmd(USBCMD_RS);
        for _ in 0..200_000 {
            if !op.usbsts().hchalted() {
                break;
            }
            crate::port::PortWriter::new(0x80).write_safe(0u8);
        }

        if op.usbsts().hchalted() {
            log::error!("xHCI: controller failed to start (HCHalted)");
            return Err("controller failed to start");
        }

        // Clear HSE
        op.clear_usbsts_bits(USBSTS_HSE);

        log::info!("xHCI: controller started");
        Ok(())
    }

    /// Clear HSE and re-kick link training on all ports.
    pub fn clear_hse_and_recover(&mut self) {
        let op = &self.registers.op;
        let sts = op.usbsts();

        if !sts.hse() {
            return;
        }

        log::info!("xHCI: HSE detected, recovering...");
        op.clear_usbsts_bits(USBSTS_HSE);

        // Force RxDetect on all ports
        for port in 0..self.ports.n_ports {
            force_rx_detect(op, port);
        }

        self.ports.clear_done_flags();

        // Wait for PHY stabilisation
        super::xhci_port::delay(1_000_000);

        // WPR on any CCS=0 powered ports, then wait for CCS
        for port_idx in 0..self.ports.n_ports {
            let ps = op.portsc(port_idx);
            if !ps.ccs() && ps.pp() {
                if let Some(p) = self.ports.get_mut(port_idx) {
                    if !p.wpr_attempted {
                        p.wpr_attempted = true;
                        let _ = warm_port_reset(op, port_idx);
                        // Wait for link training to complete after WPR
                        for _ in 0..200 {
                            super::xhci_port::delay(50_000);
                            if op.portsc(port_idx).ccs() {
                                log::info!("xHCI: port {} CCS=1 after HSE recovery WPR", port_idx);
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    // ── Port polling ───────────────────────────────────────────

    /// Poll all ports for newly connected devices.
    ///
    /// Performs power cycling, warm port reset, and port reset as needed.
    /// Returns the number of newly detected devices.
    pub fn poll_ports(&mut self) -> usize {
        let op = &self.registers.op;
        let initial_count = self.devices.len();

        // ── Check PCD (Port Change Detect) ─────────────────
        // Clear done flags on all ports when a port change is detected,
        // so that newly plugged-in devices can be enumerated.
        if op.usbsts().pcd() {
            op.clear_usbsts_bits(USBSTS_PCD);
            self.ports.clear_done_flags();
            log::info!("xHCI: PCD detected, re-evaluating all ports");
        }

        for port_idx in 0..self.ports.n_ports {
            // Skip already-processed ports
            let skip = self.ports.get(port_idx).map(|p| p.done).unwrap_or(true);
            if skip {
                continue;
            }

            self.ports.refresh_all(op);

            // Get current port state
            let port = self.ports.get(port_idx).unwrap();

            // ── Power-cycle if PPC supported ──────────────
            if self.ports.ppc && (!port.pp_on() || port.pls() == 5) {
                power_cycle(op, port_idx);
            }

            let ps = op.portsc(port_idx);
            if !ps.ccs() {
                // ── Try WPR once ─────────────────────────
                let wpr_already = self
                    .ports
                    .get(port_idx)
                    .map(|p| p.wpr_attempted)
                    .unwrap_or(true);
                if !wpr_already && ps.pp() {
                    if let Some(p) = self.ports.get_mut(port_idx) {
                        p.wpr_attempted = true;
                    }
                    let _ = warm_port_reset(op, port_idx);
                    // Wait longer for link training to complete after WPR
                    for _ in 0..120 {
                        super::xhci_port::delay(50_000);
                        if op.portsc(port_idx).ccs() {
                            break;
                        }
                    }
                }
            }

            let ps = op.portsc(port_idx);
            if !ps.ccs() {
                // ── Force RxDetect to restart link training ─
                // Even without HSE, try to kick the port into link training
                // before giving up.  Some controllers need an extra nudge.
                force_rx_detect(op, port_idx);
                super::xhci_port::delay(600_000);
                if op.portsc(port_idx).ccs() {
                    // Link training succeeded — continue to port reset below
                } else {
                    // Check for HSE
                    if op.usbsts().hse() {
                        op.clear_usbsts_bits(USBSTS_HSE);
                        force_rx_detect(op, port_idx);
                        super::xhci_port::delay(1_000_000);
                        if op.portsc(port_idx).ccs() {
                            // Continue processing below
                        } else {
                            if let Some(p) = self.ports.get_mut(port_idx) {
                                p.done = true;
                            }
                            continue;
                        }
                    } else {
                        // Still no device — don't mark done so PCD can re-trigger
                        log::debug!(
                            "xHCI: port {} no device (ccs=0, pls={}, pp={})",
                            port_idx,
                            op.portsc(port_idx).pls(),
                            if op.portsc(port_idx).pp() { 1 } else { 0 }
                        );
                        continue;
                    }
                }
            }

            // ── Port reset ────────────────────────────────
            if !op.portsc(port_idx).ped() {
                let _ = port_reset(op, port_idx);
                if !op.portsc(port_idx).ccs() {
                    continue;
                }
            }

            // ── Device detected ───────────────────────────
            let ps = op.portsc(port_idx);
            let speed = port_speed_to_usb(ps.speed());

            log::info!("xHCI: port {} device detected, speed={:?}", port_idx, speed);

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
            });
            if let Some(p) = self.ports.get_mut(port_idx) {
                p.done = true;
            }
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

    pub fn ports_done_mask(&self) -> u32 {
        self.ports.done_mask()
    }

    pub fn max_slots(&self) -> u32 {
        self.device.slots.max_slots
    }

    pub fn ppc_enabled(&self) -> bool {
        self.ports.ppc
    }

    pub fn legacy_handoff_done(&self) -> bool {
        self.legacy_handoff_done
    }

    pub fn read_cap(&self, offset: u32) -> u32 {
        self.registers.op.read(offset as usize)
    }

    pub fn read_op_reg(&self, offset: u32) -> u32 {
        self.registers.op.read(offset as usize)
    }

    pub fn read_portsc(&self, port: u32) -> u32 {
        self.registers.op.portsc(port).0
    }

    pub fn clear_devices(&mut self) {
        self.ports.clear_done_flags();
        self.devices.clear();
    }

    /// Deprecated alias — use [`clear_devices`] instead.
    pub fn clear_ports_done(&mut self) {
        self.clear_devices();
    }

    // ── HostController trait impl ─────────────────────────────

    // deferred to the impl block below

    // ── Slot management ────────────────────────────────────────

    /// Allocate a device slot.
    pub fn enable_slot(&mut self) -> Result<u32, &'static str> {
        let trb = Trb::new(trb_type::ENABLE_SLOT, self.rings.command.cycle());
        let flags = self.send_cmd(trb)?;

        // Extract slot ID from completion event (bits 24-31 of flags)
        let slot_id = ((flags >> 24) & 0xFF) as u32;

        let ctx = self.driver_ctx;
        let (slot_id, slot) = self.device.slots.alloc_slot(ctx, slot_id)?;

        // Set DCBAA entry
        self.device.dcbaa.set_slot(slot_id, slot.dev_ctx_phys);

        Ok(slot_id)
    }

    /// Address a device.
    pub fn address_device(&mut self, slot_id: u32) -> Result<(), &'static str> {
        let dev_addr = slot_id as u8;

        let ep0_ring_phys = {
            let slot = self.device.slots.get(slot_id).ok_or("bad slot")?;
            slot.ep0_ring.phys
        };

        // Set up input context
        if let Some(in_ctx) = self.device.slots.input_ctx_mut(self.driver_ctx, slot_id) {
            in_ctx.setup_address_device(dev_addr, ep0_ring_phys);
        }

        let in_ctx_phys = {
            let slot = self.device.slots.get(slot_id).ok_or("bad slot")?;
            slot.in_ctx_phys
        };

        let mut trb = Trb::new(trb_type::ADDRESS_DEVICE, self.rings.command.cycle());
        trb.set_data_ptr(in_ctx_phys);
        trb.flags |= (slot_id << 24) | trb_flag::IOC;
        self.send_cmd(trb)?;

        // Update slot state
        if let Some(slot) = self.device.slots.get_mut(slot_id) {
            slot.dev_addr = dev_addr;
        }

        // Update device address
        for dev in self.devices.iter_mut() {
            if dev.address == 0 {
                dev.address = dev_addr;
                break;
            }
        }

        Ok(())
    }

    /// Configure a bulk endpoint.
    pub fn configure_endpoint_bulk(
        &mut self,
        slot_id: u32,
        ep_addr: u8,
        mps: u16,
    ) -> Result<(), &'static str> {
        let ep_num = (ep_addr & 0x0F) as usize;
        let is_in = (ep_addr & 0x80) != 0;

        // Allocate transfer ring
        let ctx = self.driver_ctx;
        let bulk_ring = Ring::alloc(ctx, 64).ok_or("no ring")?;
        let b_phys = bulk_ring.phys;

        // Context index: EP<N> Out = 2*N, EP<N> In = 2*N+1
        let ctx_idx = 2 * ep_num + usize::from(is_in);

        // Set up input context
        if let Some(in_ctx) = self.device.slots.input_ctx_mut(self.driver_ctx, slot_id) {
            in_ctx.add_flags = (1 << ctx_idx) | 1; // Add endpoint + slot context
            in_ctx.drop_flags = 0;

            // Update Context Entries in Slot Context to the highest active endpoint index
            in_ctx.slot_ctx[0] = (in_ctx.slot_ctx[0] & !0xF800_0000) | ((ctx_idx as u32) << 27);

            if let Some(ep_ctx) = in_ctx.ep_ctx_mut(ctx_idx as u32) {
                ep_ctx[0] = (mps as u32) << 16 | 2 << 3; // MPS, type=Bulk(2)
                ep_ctx[1] = b_phys as u32;
                ep_ctx[2] = (b_phys >> 32) as u32;
                ep_ctx[3] = 0; // Average TRB Length = 0
            }
        }

        // Get in_ctx_phys before borrowing slot mutably
        let in_ctx_phys = {
            let slot = self.device.slots.get(slot_id).ok_or("bad slot")?;
            slot.in_ctx_phys
        };

        // Store the ring
        if let Some(slot) = self.device.slots.get_mut(slot_id) {
            if is_in {
                slot.bulk_in_ring = Some(bulk_ring);
            } else {
                slot.bulk_out_ring = Some(bulk_ring);
            }
        }

        // Build Configure Endpoint TRB
        let mut trb = Trb::new(trb_type::CONFIGURE_ENDPOINT, self.rings.command.cycle());
        trb.set_data_ptr(in_ctx_phys);
        trb.flags |= (slot_id << 24) | trb_flag::IOC;
        self.send_cmd(trb)?;

        Ok(())
    }

    /// Release a single device slot and free its resources.
    pub fn disable_slot(&mut self, slot_id: u32) {
        let ctx = self.driver_ctx;
        self.device.dcbaa.clear_slot(slot_id);
        self.device.slots.release_slot(slot_id, ctx);
    }

    /// Release all device slots and free resources.
    pub fn disable_all_slots(&mut self) {
        let ctx = self.driver_ctx;
        for slot in &self.device.slots.slots {
            self.device.dcbaa.clear_slot(slot.slot_id);
        }
        self.device.slots.release_all(ctx);
    }

    // ── Command submission ─────────────────────────────────────

    /// Enqueue a command TRB and wait for completion.
    fn send_cmd(&mut self, trb: Trb) -> Result<u32, &'static str> {
        self.rings.command.enqueue(trb);
        self.registers.doorbell.ring(0, 0);
        let ev = wait_event(&mut self.rings.event, &self.registers.runtime, 5_000_000)?;
        Ok(ev.flags)
    }

    /// Wait for an event with timeout.
    pub fn wait_event(&mut self, timeout: u32) -> Result<Trb, &'static str> {
        wait_event(&mut self.rings.event, &self.registers.runtime, timeout)
    }

    // ── Control transfer ───────────────────────────────────────

    /// Perform a control transfer on EP0.
    pub fn control_transfer(
        &mut self,
        slot_id: u32,
        setup: &UsbSetupPacket,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        let is_in = (setup.bm_request_type & 0x80) != 0;
        let data_len = setup.w_length as usize;
        if data_len > buf.len() {
            return Err("control buffer too small");
        }

        // Check slot validity
        let _ep0_cycle = {
            let slot = self.device.slots.get(slot_id).ok_or("bad slot")?;
            slot.ep0_ring.cycle
        };

        // Allocate staging buffer
        let staging_phys = if data_len > 0 {
            self.driver_ctx
                .allocate_contiguous_frames((data_len + 4095) / 4096)
                .map_err(|_| "no staging memory")?
        } else {
            0
        };
        let staging_virt = if staging_phys != 0 {
            self.driver_ctx.phys_to_virt(staging_phys) as *mut u8
        } else {
            core::ptr::null_mut()
        };

        // Copy OUT data
        if data_len > 0 && !is_in {
            unsafe {
                ptr::copy_nonoverlapping(buf.as_ptr(), staging_virt, data_len);
            }
        }

        // Build TRB chain on EP0 ring
        let setup_bytes =
            unsafe { core::slice::from_raw_parts(setup as *const UsbSetupPacket as *const u8, 8) };

        if let Some(slot) = self.device.slots.get_mut(slot_id) {
            // SETUP TRB
            let mut s_trb = Trb::new(trb_type::SETUP_STAGE, slot.ep0_ring.cycle);
            s_trb.params[..8].copy_from_slice(setup_bytes);
            let trt = if data_len == 0 {
                0u32
            } else if is_in {
                2 << 16
            } else {
                3 << 16
            };
            s_trb.flags |= trb_flag::CHAIN | trt;
            slot.ep0_ring.enqueue(s_trb);

            // DATA TRB (if any)
            if data_len > 0 {
                let mut d_trb = Trb::new(trb_type::DATA_STAGE, slot.ep0_ring.cycle);
                d_trb.set_data_ptr(staging_phys);
                d_trb.set_transfer_length(data_len as u32);
                if is_in {
                    d_trb.flags |= trb_flag::DIR_IN | trb_flag::CHAIN;
                } else {
                    d_trb.flags |= trb_flag::CHAIN;
                }
                slot.ep0_ring.enqueue(d_trb);
            }

            // STATUS TRB
            let mut st_trb = Trb::new(trb_type::STATUS_STAGE, slot.ep0_ring.cycle);
            if !is_in || data_len == 0 {
                st_trb.flags |= trb_flag::DIR_IN;
            }
            st_trb.flags |= trb_flag::IOC;
            slot.ep0_ring.enqueue(st_trb);
        }

        // Doorbell EP0 (DCI=1)
        self.registers.doorbell.ring(slot_id, 1);
        let res = self.wait_event(5_000_000);

        // Copy IN data back
        if res.is_ok() && is_in && data_len > 0 {
            unsafe {
                ptr::copy_nonoverlapping(staging_virt, buf.as_mut_ptr(), data_len);
            }
        }

        // Free staging buffer (only on success, HC may still own it on timeout)
        if res.is_ok() && staging_phys != 0 {
            self.driver_ctx
                .free_contiguous_frames(staging_phys, (data_len + 4095) / 4096);
        }

        res.map(|_| data_len)
    }

    // ── Bulk transfer ──────────────────────────────────────────

    /// Perform a bulk transfer.
    pub fn bulk_transfer(
        &mut self,
        slot_id: u32,
        endpoint: u8,
        buf: &mut [u8],
        dir: UsbDirection,
        _mps: u16,
    ) -> Result<usize, &'static str> {
        if buf.len() > 65536 {
            return Err("bulk transfer too large");
        }
        if buf.is_empty() {
            return Ok(0);
        }
        let len = buf.len();

        // Validate slot and ring existence BEFORE allocating staging buffer
        {
            let slot = self.device.slots.get(slot_id).ok_or("bad slot")?;
            match dir {
                UsbDirection::In => {
                    let _ = slot.bulk_in_ring.as_ref().ok_or("no bulk in ring")?;
                }
                UsbDirection::Out => {
                    let _ = slot.bulk_out_ring.as_ref().ok_or("no bulk out ring")?;
                }
            }
        }

        // Allocate staging buffer
        let staging_pages = (len + 4095) / 4096;
        let staging_phys = self
            .driver_ctx
            .allocate_contiguous_frames(staging_pages)
            .map_err(|_| "no staging memory")?;
        let staging_virt = self.driver_ctx.phys_to_virt(staging_phys) as *mut u8;

        // Copy OUT data
        if dir == UsbDirection::Out {
            unsafe {
                ptr::copy_nonoverlapping(buf.as_ptr(), staging_virt, len);
            }
        }

        // Enqueue TRB
        let db_stream = {
            let slot = self.device.slots.get_mut(slot_id).unwrap();
            let ring = match dir {
                UsbDirection::In => slot.bulk_in_ring.as_mut().unwrap(),
                UsbDirection::Out => slot.bulk_out_ring.as_mut().unwrap(),
            };

            let mut trb = Trb::new(trb_type::NORMAL, ring.cycle);
            trb.set_data_ptr(staging_phys);
            trb.set_transfer_length(len as u32);
            if dir == UsbDirection::In {
                trb.flags |= trb_flag::DIR_IN;
            }
            trb.flags |= trb_flag::IOC | trb_flag::ENT;
            ring.enqueue(trb);

            let ep_num = (endpoint & 0x0F) as u32;
            let is_in = (endpoint & 0x80) != 0;
            ep_num * 2 + u32::from(is_in)
        };

        self.registers.doorbell.ring(slot_id, db_stream);
        let res = self.wait_event(5_000_000);

        // Copy IN data back on success; free staging buffer unconditionally
        if res.is_ok() && dir == UsbDirection::In {
            unsafe {
                ptr::copy_nonoverlapping(staging_virt, buf.as_mut_ptr(), len);
            }
        }
        self.driver_ctx
            .free_contiguous_frames(staging_phys, staging_pages);

        res.map(|_| len)
    }

    // ── Descriptor helpers ─────────────────────────────────────

    /// Get device descriptor (18 bytes).
    pub fn get_device_descriptor(
        &mut self,
        slot_id: u32,
    ) -> Result<crate::usb::UsbDeviceDescriptor, &'static str> {
        let mut buf = [0u8; 18];
        let setup = UsbSetupPacket {
            bm_request_type: 0x80,
            b_request: crate::usb::REQ_GET_DESCRIPTOR,
            w_value: (crate::usb::DESC_DEVICE as u16) << 8,
            w_index: 0,
            w_length: 18,
        };
        self.control_transfer(slot_id, &setup, &mut buf)?;
        let desc =
            unsafe { ptr::read_unaligned(buf.as_ptr() as *const crate::usb::UsbDeviceDescriptor) };
        Ok(desc)
    }

    /// Set device address.
    pub fn set_address(&mut self, slot_id: u32, addr: u8) -> Result<(), &'static str> {
        let setup = UsbSetupPacket {
            bm_request_type: 0x00,
            b_request: crate::usb::REQ_SET_ADDRESS,
            w_value: addr as u16,
            w_index: 0,
            w_length: 0,
        };
        self.control_transfer(slot_id, &setup, &mut [])?;
        Ok(())
    }

    /// Set device configuration.
    pub fn set_configuration(
        &mut self,
        slot_id: u32,
        config_value: u8,
    ) -> Result<(), &'static str> {
        let setup = UsbSetupPacket {
            bm_request_type: 0x00,
            b_request: crate::usb::REQ_SET_CONFIGURATION,
            w_value: config_value as u16,
            w_index: 0,
            w_length: 0,
        };
        self.control_transfer(slot_id, &setup, &mut [])?;
        Ok(())
    }

    // ── PCI creation ───────────────────────────────────────────

    /// Create from a PCI device configuration.
    pub fn from_pci(
        device: &crate::pci::PciDevice,
        ctx: &'static dyn DriverContext,
    ) -> Option<Self> {
        let mmio_phys = device.read_bar(0)?;
        if mmio_phys == 0 {
            return None;
        }
        let mmio_virt = ctx.phys_to_virt(mmio_phys) as *mut u8;
        XhciContext::new(mmio_virt, ctx)
    }
}

// ============================================================================
//  HostController trait impl for XhciContext
// ============================================================================

impl HostController for XhciContext {
    fn reset(&mut self) -> Result<(), &'static str> {
        self.controller_reset()
    }
    fn start(&mut self) -> Result<(), &'static str> {
        self.start_controller()
    }
    fn poll_ports(&mut self) -> usize {
        self.poll_ports()
    }
    fn clear_devices(&mut self) {
        self.clear_devices()
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
        self.control_transfer(dev_addr as u32, setup, buf)
    }
    fn bulk_transfer(
        &mut self,
        dev_addr: u8,
        endpoint: u8,
        buf: &mut [u8],
        dir: UsbDirection,
        mps: u16,
    ) -> Result<usize, &'static str> {
        self.bulk_transfer(dev_addr as u32, endpoint, buf, dir, mps)
    }
}

impl Drop for XhciContext {
    fn drop(&mut self) {
        self.disable_all_slots();
        self.rings.command.ring.free(self.driver_ctx);
        self.rings.event.free(self.driver_ctx);

        // Free DCBAA page
        let _ = self
            .driver_ctx
            .free_contiguous_frames(self.device.dcbaa.phys, 1);

        // Free Scratchpad array and buffer pages
        if let Some(ref sp) = self.device.scratchpad {
            let array_virt = self.driver_ctx.phys_to_virt(sp.phys) as *const u64;
            for i in 0..sp.count as usize {
                let buf_phys = unsafe { ptr::read_volatile(array_virt.add(i)) };
                let _ = self.driver_ctx.free_contiguous_frames(buf_phys, 1);
            }
            let array_pages = ((sp.count as usize * 8) + 4095) / 4096;
            let _ = self.driver_ctx.free_contiguous_frames(sp.phys, array_pages);
        }
    }
}

// ============================================================================
//  Tests
// ============================================================================

#[cfg(test)]
mod tests {
    // Tests are in sub-modules xhci_ring, xhci_register, xhci_port.
}
