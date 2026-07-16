//! xHCI controller construction, startup, recovery, and root-port lifecycle.

use alloc::vec::Vec;
use core::ptr;

use super::context::XhciContext;
use super::device::DeviceContextSet;
use super::interrupt::InterruptContext;
use super::port::{MAX_PORT_RETRIES, PortContext, delay_ms, delay_us, ensure_port_ready};
use super::register::{
    CapabilityRegisters, DoorbellRegisters, OP_PORTSC_BASE, OP_PORTSC_STRIDE, OperationalRegisters,
    RT_INTERRUPTER_STRIDE, RegisterContext, RuntimeRegisters, USBCMD_HCRST, USBCMD_HSEE,
    USBCMD_INTE, USBCMD_RS, USBSTS_CNR, USBSTS_HCH, USBSTS_HSE, USBSTS_PCD, parse_port_protocols,
    port_speed_to_usb, try_legacy_handoff,
};
use super::ring::{ErstEntry, RingContext};
use crate::DriverContext;
use crate::pci_health::PciHealth;
use crate::usb::UsbDevice;

impl XhciContext {
    /// Create a new xHCI context from the MMIO base address.
    ///
    /// This reads capability registers, performs legacy handoff, and allocates
    /// the rings, device contexts, and port state owned by the controller.
    ///
    /// # Safety
    ///
    /// `mmio_base` must reference a mapped xHCI register BAR for the lifetime
    /// of the returned controller.
    pub unsafe fn new(
        mmio_base: *mut u8,
        ctx: &'static dyn DriverContext,
        health: PciHealth,
    ) -> Option<Self> {
        crate::timing::delay_us(100);

        crate::debug::hint(b"xh_cap");
        let Some(cap_regs) = (unsafe { CapabilityRegisters::read(mmio_base) }) else {
            log::warn!("xHCI: invalid or inaccessible capability header");
            return None;
        };
        crate::debug::hint(b"xh_capok");

        let op_off = cap_regs.caplength as usize;
        let rt_off = cap_regs.rt_offset as usize;
        let db_off = cap_regs.db_offset as usize;
        let hcs1 = cap_regs.hcs_params1();
        let hcc1 = cap_regs.hcc_params1();
        let n_ports = hcs1.n_ports;
        let max_slots = hcs1.max_slots;
        let ppc = hcs1.ppc;
        let scratchpad_bufs = hcs1.max_scratchpad_bufs;

        if n_ports == 0 || max_slots == 0 || cap_regs.db_offset == 0 || cap_regs.rt_offset == 0 {
            log::warn!("xHCI: invalid or inaccessible capability registers");
            return None;
        }

        let bar_size = crate::usb::HOST_CONTROLLER_BAR_SIZE;
        let in_bar =
            |offset: usize, len: usize| offset.checked_add(len).is_some_and(|end| end <= bar_size);
        let op_len = OP_PORTSC_BASE
            .checked_add(n_ports as usize * OP_PORTSC_STRIDE)
            .and_then(|len| len.checked_add(4));
        let rt_len = RT_INTERRUPTER_STRIDE * 2;
        let db_len = (max_slots as usize + 1) * 4;
        if op_len.is_none_or(|len| !in_bar(op_off, len))
            || !in_bar(rt_off, rt_len)
            || !in_bar(db_off, db_len)
        {
            log::warn!("xHCI: capability offsets exceed mapped BAR window");
            return None;
        }

        log::info!(
            "xHCI: HCSPARAMS1=0x{:08X} n_ports={} max_slots={} ppc={} scratchpad={}",
            cap_regs.hcs_params1,
            n_ports,
            max_slots,
            ppc,
            scratchpad_bufs,
        );
        log::info!(
            "xHCI: HCCPARAMS1=0x{:08X} 64bit={} xECP=0x{:x} CSZ={} PPC={}",
            cap_regs.hcc_params1,
            hcc1.ac64,
            hcc1.ext_cap_ptr,
            hcc1.csz,
            hcc1.ppc,
        );
        log::info!("xHCI: HCIVERSION=0x{:04X}", cap_regs.hci_version);

        crate::debug::hint(b"xh_leg");
        let legacy_ok = match try_legacy_handoff(mmio_base, hcc1.ext_cap_ptr) {
            Ok(true) | Ok(false) => true,
            Err(_) => {
                log::info!("xHCI: legacy handoff failed");
                return None;
            }
        };
        crate::debug::hint(b"xh_legok");

        let op_base = unsafe { mmio_base.add(op_off) };
        let rt_base = unsafe { mmio_base.add(rt_off + RT_INTERRUPTER_STRIDE) };
        let db_base = unsafe { mmio_base.add(db_off) };
        let registers = RegisterContext {
            mmio_base,
            cap: cap_regs,
            op: unsafe { OperationalRegisters::new(op_base) },
            runtime: unsafe { RuntimeRegisters::new(rt_base) },
            doorbell: unsafe { DoorbellRegisters::new(db_base) },
        };

        crate::debug::hint(b"xh_dma");
        let rings = RingContext::alloc(ctx, 256, 256)?;
        let device = DeviceContextSet::new(ctx, max_slots, scratchpad_bufs)?;
        let port_protocols = parse_port_protocols(mmio_base, hcc1.ext_cap_ptr, n_ports);
        let ports = PortContext::new(n_ports, ppc, Some(&port_protocols));
        let interrupts = InterruptContext::new();

        let controller = Self {
            registers,
            rings,
            device,
            ports,
            interrupts,
            devices: Vec::new(),
            driver_ctx: ctx,
            health,
            legacy_handoff_done: legacy_ok,
            erst_phys: None,
            deferred_free_list: Vec::new(),
        };
        crate::debug::hint(b"xh_newok");
        Some(controller)
    }

    /// Initialise the controller, install DMA structures, start it, and
    /// initialise root-hub ports.
    pub fn init(&mut self) -> Result<(), crate::DriverError> {
        log::info!("xHCI: hci_version=0x{:04X}", self.registers.cap.hci_version);

        if !self.health.is_device_present() {
            log::error!("xHCI: device gone before init");
            return Err(crate::DriverError::DeviceNotFound);
        }

        crate::debug::hint(b"xh_cnr");
        if crate::timing::wait_timeout_us(5_000_000, || {
            self.registers.op.usbsts() & USBSTS_CNR == 0
        })
        .is_err()
        {
            return Err(crate::DriverError::NotReady);
        }

        let sts = self.registers.op.usbsts();
        if sts & USBSTS_HCH == 0 {
            log::info!("xHCI: controller running, halting before HCRST");
            self.registers
                .op
                .set_usbcmd(self.registers.op.usbcmd() & !(USBCMD_RS | USBCMD_INTE | USBCMD_HSEE));
            if crate::timing::wait_timeout_us(500_000, || {
                self.registers.op.usbsts() & USBSTS_HCH != 0
            })
            .is_err()
            {
                return Err(crate::DriverError::DeviceFault);
            }
        }

        crate::debug::hint(b"xh_reset");
        self.controller_reset()?;
        self.configure_before_start();
        self.setup_erst()?;
        self.interrupts.enable(&self.registers.runtime);
        self.registers.op.set_usbcmd_bits(USBCMD_INTE);

        if self.registers.op.usbsts() & USBSTS_HSE != 0 {
            log::warn!("xHCI: HSE after HCRST, clearing");
            self.clear_hse_and_recover();
        }

        crate::debug::hint(b"xh_start");
        self.start_controller()?;
        self.clear_hse_and_recover();
        crate::debug::hint(b"xh_ports");
        self.init_ports();
        crate::debug::hint(b"xh_ready");
        Ok(())
    }

    /// Reset the controller with HCRST.
    pub fn reset(&mut self) -> Result<(), crate::DriverError> {
        self.controller_reset()
    }

    /// Start the controller and wait for HCHalted to clear.
    pub fn start(&mut self) -> Result<(), crate::DriverError> {
        self.start_controller()
    }

    /// Clear HSE and re-kick link training on all ports.
    pub fn clear_hse_and_recover(&mut self) {
        let op = &self.registers.op;
        if op.usbsts() & USBSTS_HSE == 0 {
            return;
        }

        log::info!("xHCI: HSE detected, recovering...");
        op.clear_usbsts_bits(USBSTS_HSE);
        self.ports.clear_done_flags();
        delay_ms(200);

        for port_idx in 0..self.ports.n_ports {
            let is_usb3 = self.ports.get(port_idx).map(|p| p.is_usb3).unwrap_or(true);
            ensure_port_ready(op, port_idx, is_usb3, self.ports.ppc, false);
        }
    }

    /// Poll all ports for newly connected devices.
    pub fn poll_ports(&mut self) -> usize {
        let mut added = 0usize;

        if self.registers.op.usbsts() & USBSTS_PCD != 0 {
            self.registers.op.clear_usbsts_bits(USBSTS_PCD);
            let pre_ccs: Vec<bool> = (0..self.ports.n_ports)
                .map(|i| self.ports.get(i).map(|p| p.ccs()).unwrap_or(false))
                .collect();
            self.ports.refresh_all(&self.registers.op);

            for port_idx in 0..self.ports.n_ports {
                let ccs = self.ports.get(port_idx).map(|p| p.ccs()).unwrap_or(false);
                let was = pre_ccs.get(port_idx as usize).copied().unwrap_or(false);
                if ccs != was {
                    if let Some(port) = self.ports.get_mut(port_idx) {
                        port.done = false;
                        port.wpr_attempted = false;
                        port.retry_count = 0;
                        log::info!(
                            "xHCI: port {} CCS changed ({} -> {}), re-evaluating",
                            port_idx,
                            was,
                            ccs
                        );
                    }
                    self.devices.retain(|device| device.port_index != port_idx);
                }
            }
        } else {
            self.ports.refresh_all(&self.registers.op);
        }

        for port_idx in 0..self.ports.n_ports {
            if self.ports.get(port_idx).map(|p| p.done).unwrap_or(true) {
                continue;
            }

            if !self.try_connect_port(port_idx) {
                if !self.registers.op.portsc(port_idx).ccs() {
                    self.devices.retain(|device| device.port_index != port_idx);
                    log::info!("xHCI: port {} disconnected", port_idx);
                }
                continue;
            }

            let ps = self.registers.op.portsc(port_idx);
            let speed = port_speed_to_usb(ps.speed());
            log::info!("xHCI: port {} device detected, speed={:?}", port_idx, speed);
            self.devices.retain(|device| device.port_index != port_idx);
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
            added += 1;
            if let Some(port) = self.ports.get_mut(port_idx) {
                port.done = true;
            }
        }

        added
    }

    fn log_portsc(&self, port_idx: u32) {
        let ps = self.registers.op.portsc(port_idx);
        log::info!(
            "xHCI:   PORTSC[{}]={:#010X} CCS={} PED={} PLS={} PP={} PR={} WPR={} speed={} \
             CSC={} PEC={} WRC={} PRC={} PLC={} OCC={} CEC={}",
            port_idx,
            ps.0,
            ps.ccs() as u32,
            ps.ped() as u32,
            ps.pls(),
            ps.pp() as u32,
            ps.pr() as u32,
            ps.wpr() as u32,
            ps.speed(),
            (ps.0 >> 17) & 1,
            (ps.0 >> 18) & 1,
            (ps.0 >> 19) & 1,
            (ps.0 >> 21) & 1,
            (ps.0 >> 22) & 1,
            (ps.0 >> 20) & 1,
            (ps.0 >> 23) & 1,
        );
    }

    fn init_ports(&mut self) {
        let op = &self.registers.op;
        log::info!("xHCI: initialising {} ports", self.ports.n_ports);

        for port_idx in 0..self.ports.n_ports {
            let is_usb3 = self.ports.get(port_idx).map(|p| p.is_usb3).unwrap_or(true);
            let ready = ensure_port_ready(op, port_idx, is_usb3, self.ports.ppc, false);
            if ready {
                log::info!("xHCI: port {} ready after init_ports", port_idx);
            } else {
                log::info!(
                    "xHCI: port {} not ready (portsc=0x{:08X})",
                    port_idx,
                    op.portsc(port_idx).0
                );
            }
            self.log_portsc(port_idx);
        }
        log::info!("xHCI: port initialisation complete");
    }

    fn controller_reset(&mut self) -> Result<(), crate::DriverError> {
        let op = &self.registers.op;
        let usbcmd = op.usbcmd();
        let sts = op.usbsts();
        let already_running = sts & USBSTS_HCH == 0;
        log::info!(
            "xHCI: USBCMD=0x{:08X} USBSTS=0x{:08X} HCHalted={} already_running={}",
            usbcmd,
            sts,
            (sts & USBSTS_HCH) != 0,
            already_running
        );
        log::info!("xHCI: performing HCRST");

        op.set_usbcmd(USBCMD_HCRST);
        delay_us(1000);

        if crate::timing::wait_timeout_us(500_000, || op.usbcmd() & USBCMD_HCRST == 0).is_err() {
            log::warn!("xHCI: HCRST did not clear");
            return Err(crate::DriverError::TimedOut);
        }
        if crate::timing::wait_timeout_us(500_000, || op.usbsts() & USBSTS_HCH != 0).is_err() {
            log::warn!("xHCI: controller did not halt after HCRST");
            return Err(crate::DriverError::TimedOut);
        }
        if crate::timing::wait_timeout_us(500_000, || op.usbsts() & USBSTS_CNR == 0).is_err() {
            log::warn!("xHCI: CNR did not clear after HCRST");
            return Err(crate::DriverError::TimedOut);
        }

        let sts_after = op.usbsts();
        log::info!(
            "xHCI: after HCRST, USBSTS=0x{:08X} HCHalted={} CNR={}",
            sts_after,
            (sts_after & USBSTS_HCH) != 0,
            (sts_after & USBSTS_CNR) != 0
        );
        Ok(())
    }

    fn configure_before_start(&mut self) {
        let op = &self.registers.op;
        op.set_dcbaap(self.device.dcbaa.phys);
        op.set_crcr(self.rings.command.phys | 1);
        op.set_config(self.device.slots.max_slots);
        op.set_usbcmd_bits(USBCMD_HSEE);
    }

    fn setup_erst(&mut self) -> Result<(), crate::DriverError> {
        let rt = &self.registers.runtime;
        let ctx = self.driver_ctx;
        let erst_phys = if let Some(phys) = self.erst_phys {
            phys
        } else {
            let phys = ctx
                .allocate_contiguous_frames(1)
                .map_err(|_| crate::DriverError::OutOfMemory)?;
            self.erst_phys = Some(phys);
            phys
        };
        let erst_virt = ctx.phys_to_virt(erst_phys) as *mut ErstEntry;
        unsafe {
            ptr::write_volatile(erst_virt, ErstEntry::new(self.rings.event.phys, 256));
        }
        rt.set_erstsz(1);
        rt.set_erstba(erst_phys);
        rt.set_erdp(self.rings.event.dequeue_ptr());
        Ok(())
    }

    fn start_controller(&mut self) -> Result<(), crate::DriverError> {
        let op = &self.registers.op;
        op.set_usbcmd_bits(USBCMD_RS | USBCMD_HSEE);
        if crate::timing::wait_timeout_us(500_000, || op.usbsts() & USBSTS_HCH == 0).is_err() {
            log::error!("xHCI: controller failed to start (HCHalted)");
            return Err(crate::DriverError::DeviceFault);
        }
        log::info!("xHCI: controller started");
        Ok(())
    }

    fn try_connect_port(&mut self, port_idx: u32) -> bool {
        let op = &self.registers.op;
        if let Some(port) = self.ports.get_mut(port_idx) {
            port.refresh(op);
        }

        let is_usb3 = self.ports.get(port_idx).map(|p| p.is_usb3).unwrap_or(true);
        let wpr_done = if is_usb3 && !op.portsc(port_idx).ccs() {
            self.ports
                .get(port_idx)
                .map(|port| port.wpr_attempted)
                .unwrap_or(true)
        } else {
            true
        };
        if !wpr_done && let Some(port) = self.ports.get_mut(port_idx) {
            port.wpr_attempted = true;
        }

        if ensure_port_ready(op, port_idx, is_usb3, self.ports.ppc, wpr_done) {
            return true;
        }

        if op.usbsts() & USBSTS_HSE != 0 {
            op.clear_usbsts_bits(USBSTS_HSE);
            delay_ms(300);
            if ensure_port_ready(op, port_idx, is_usb3, self.ports.ppc, wpr_done) {
                return true;
            }
        }

        if let Some(port) = self.ports.get_mut(port_idx) {
            port.retry_count = port.retry_count.saturating_add(1);
            if port.retry_count >= MAX_PORT_RETRIES {
                port.done = true;
                log::debug!(
                    "xHCI: port {} done after {} retries",
                    port_idx,
                    port.retry_count
                );
            } else {
                log::debug!(
                    "xHCI: port {} no device (ccs={} pls={} pp={} retry={})",
                    port_idx,
                    op.portsc(port_idx).ccs() as u32,
                    op.portsc(port_idx).pls(),
                    op.portsc(port_idx).pp() as u32,
                    port.retry_count,
                );
            }
        }
        false
    }
}
