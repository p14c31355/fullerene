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
use super::device::*;
use crate::usb::host_controller::HostController;
use super::interrupt::*;
use super::port::*;
use super::register::*;
use super::ring::*;

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
    /// ERST physical address (allocated in configure_registers).
    erst_phys: Option<u64>,
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
        log::info!(
            "xHCI: HCIVERSION=0x{:04X}",
            cap_regs.hci_version,
        );

        // ── Step 2: Dump extended capabilities ────────────────
        if hcc1.ext_cap_ptr != 0 {
            dump_extended_capabilities(mmio_base, hcc1.ext_cap_ptr);
        }

        // ── Step 3: Legacy handoff ────────────────────────────
        let legacy_ok = match try_legacy_handoff(mmio_base, hcc1.ext_cap_ptr) {
            Ok(true) => true,  // OS already owns
            Ok(false) => true, // handoff succeeded
            Err(_) => {
                log::info!("xHCI: legacy handoff failed");
                return None;
            }
        };

        // ── Step 4: Create sub-contexts ───────────────────────
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
        let port_protocols = parse_port_protocols(mmio_base, hcc1.ext_cap_ptr, n_ports);
        let ports = PortContext::new(n_ports, ppc, Some(&port_protocols));
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
            erst_phys: None,
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
        self.registers.op.usbsts()
    }

    /// Check if the controller is running (HCHalted = 0).
    pub fn is_running(&self) -> bool {
        self.registers.op.usbsts() & USBSTS_HCH == 0
    }

    // ── Initialisation ─────────────────────────────────────────

    /// Initialise the controller: reset, configure registers, start.
    ///
    /// For xHCI 1.0 controllers (HCIVERSION < 0x0110) we skip HCRST
    /// and preserve the firmware port state, because they lack WPR
    /// and may reject PR with CCS=0, making it impossible to recover
    /// from RxDetect after a full reset.
    pub fn init(&mut self) -> Result<(), &'static str> {
        let hci_ver = self.registers.cap.hci_version;
        if hci_ver < 0x0110 {
            log::info!("xHCI: hci_version=0x{:04X} < 0x0110, using init_no_reset", hci_ver);
            return self.init_no_reset();
        }
        self.controller_reset()?;
        self.configure_registers()?;
        self.start_controller_no_inte()?;
        self.enable_interrupts();
        self.clear_hse_and_recover();
        self.init_ports();
        Ok(())
    }

    /// Lightweight init without HCRST — preserves firmware port state.
    ///
    /// Some xHCI 1.0 controllers cannot recover from RxDetect after HCRST
    /// because they lack WPR support and reject PR when CCS=0.
    /// This path skips the reset and uses the firmware's already-detected
    /// device state.  Ports that already have CCS=1 will be usable.
    pub fn init_no_reset(&mut self) -> Result<(), &'static str> {
        log::info!("xHCI: init_no_reset — skipping HCRST, preserving firmware state");

        // ── Diagnostic: controller status (separate block) ────
        {
            let sts = self.registers.op.usbsts();
            log::info!(
                "xHCI: USBSTS={:#010X} HCHalted={} HSE={} EINT={} PCD={} CNR={} HCE={}",
                sts, ((sts & USBSTS_HCH) != 0) as u32, ((sts & USBSTS_HSE) != 0) as u32, ((sts & USBSTS_EINT) != 0) as u32,
                ((sts & USBSTS_PCD) != 0) as u32, ((sts & USBSTS_CNR) != 0) as u32, ((sts & USBSTS_HCE) != 0) as u32,
            );
        }

        // Step 1: Start controller FIRST, before configuring registers.
        // On Intel Wildcat Point xHCI 1.0, RS=1 must be set before the
        // PHY begins link training.  Waiting between RS=1 and register
        // configuration gives the internal state machine time to stabilise.
        if self.registers.op.usbsts() & USBSTS_HCH != 0 {
            log::info!("xHCI: controller is halted, starting it FIRST");
            self.start_controller_no_inte()?;
            // Allow PHY to stabilise before writing context/ring registers
            log::info!("xHCI: waiting 500ms after RS=1 for PHY stabilisation");
            super::port::delay_ms(500);
        } else {
            log::info!("xHCI: controller already running");
            self.registers.op.set_usbcmd_bits(USBCMD_HSEE);
        }

        // Step 2: Now configure registers (DCBAAP, CRCR, CONFIG, ERST).
        log::info!("xHCI: configuring registers after RS=1");
        self.configure_registers()?;
        self.enable_interrupts();
        super::port::delay_ms(200);

        // Step 3: Reflect current firmware-detected ports.
        let n_ports = self.ports.n_ports;
        {
            let op = &self.registers.op;
            for port in &mut self.ports.ports {
                port.refresh(op);
            }
        }
        let mut found = 0;
        for port_idx in 0..n_ports {
            self.log_portsc(port_idx);
            if self.registers.op.portsc(port_idx).ccs() {
                found += 1;
                log::info!("xHCI: port {} already has CCS=1 (firmware detected)", port_idx);
            }
        }
        if found == 0 {
            log::warn!("xHCI: no ports with CCS=1 after RS-first init, trying init_ports fallback");
            self.init_ports();

            // If init_ports still found nothing, try a full HCRST +
            // re-init.  Some xHCI 1.0 controllers (Wildcat Point etc.)
            // lose the port state when the controller is halted during
            // ExitBootServices.  A full reset + re-init forces the PHY
            // to go through the full RxDetect→Polling→U0 sequence.
            let still_empty = (0..n_ports).all(|i| !self.registers.op.portsc(i).ccs());
            if still_empty {
                log::warn!("xHCI: init_ports produced no CCS=1, trying full HCRST");
                self.controller_reset()?;
                self.configure_registers()?;
                self.start_controller_no_inte()?;
                self.enable_interrupts();
                self.clear_hse_and_recover();
                self.init_ports();
                let after_hcrst = (0..n_ports).any(|i| self.registers.op.portsc(i).ccs());
                if !after_hcrst {
                    log::warn!("xHCI: even HCRST did not produce CCS=1");
                }
            }
        }
        Ok(())
    }

    /// Log PORTSC value for a single port with all relevant fields.
    fn log_portsc(&self, port_idx: u32) {
        let ps = self.registers.op.portsc(port_idx);
        log::info!(
            "xHCI:   PORTSC[{}]={:#010X} CCS={} PED={} PLS={} PP={} PR={} WPR={} speed={} \
             CSC={} PEC={} WRC={} PRC={} PLC={} OCC={} CEC={}",
            port_idx, ps.0,
            ps.ccs() as u32, ps.ped() as u32, ps.pls(), ps.pp() as u32,
            ps.pr() as u32, ps.wpr() as u32, ps.speed(),
            (ps.0 >> 17) & 1, (ps.0 >> 18) & 1, (ps.0 >> 19) & 1,
            (ps.0 >> 21) & 1, (ps.0 >> 22) & 1, (ps.0 >> 20) & 1,
            (ps.0 >> 23) & 1,
        );
    }

    /// Initialise all root-hub ports: ensure port power is on, kick
    /// link training via RxDetect, and wait for devices to appear
    /// (CCS=1).  This is called once after the controller starts.
    fn init_ports(&mut self) {
        let op = &self.registers.op;

        log::info!("xHCI: initialising {} ports", self.ports.n_ports);

        // ── Diagnostic: dump initial port states ──────────────
        (0..self.ports.n_ports).for_each(|p| self.log_portsc(p));

        // ── Power up: USB 3.0 power-cycle, then PP=1 x2 on all ──
        for port_idx in 0..self.ports.n_ports {
            let ps = op.portsc(port_idx).0;
            let is_usb3 = self.ports.get(port_idx).map(|p| p.is_usb3).unwrap_or(true);
            if is_usb3 && (ps & PORTSC_PP) != 0 {
                op.write_portsc(port_idx, (ps & !PORTSC_RW1C_MASK) & !PORTSC_PP);
                super::port::delay_ms(20);
                op.write_portsc(port_idx, (op.portsc(port_idx).0 & !PORTSC_RW1C_MASK) | PORTSC_PP);
                super::port::delay_ms(100);
            }
        }
        for _ in 0..2 {
            for p in 0..self.ports.n_ports {
                op.write_portsc(p, (op.portsc(p).0 & !PORTSC_RW1C_MASK) | PORTSC_PP);
            }
            super::port::delay_ms(20);
        }
        super::port::delay_ms(50);
        (0..self.ports.n_ports).for_each(|p| self.log_portsc(p));

        // ── Exit Compliance + kick RxDetect on all ports ──────
        for port_idx in 0..self.ports.n_ports {
            super::port::exit_compliance(op, port_idx);
        }
        const PLS_RXDETECT: u32 = 5 << 5;
        for port_idx in 0..self.ports.n_ports {
            op.update_portsc(port_idx, PLS_RXDETECT | PORTSC_LWS, PORTSC_PLS_MASK | PORTSC_LWS);
        }
        for port_idx in 0..self.ports.n_ports {
            let ps = op.portsc(port_idx).0;
            if ps & PORTSC_RW1C_MASK != 0 {
                op.write_portsc(port_idx, (ps & !PORTSC_RW1C_MASK) | (ps & PORTSC_RW1C_MASK));
            }
        }
        super::port::delay_ms(200);

        // ── Step 5: wait for link training to complete ──────────
        // USB 3.0 link training can take 100–500ms.  We poll CCS
        // for up to ~2 s.  Also try USB 2.0 ports.
        for port_idx in 0..self.ports.n_ports {
            for _ in 0..200 {
                super::port::delay_ms(10);
                if op.portsc(port_idx).ccs() {
                    log::info!("xHCI: port {} CCS=1 after init_ports", port_idx);
                    break;
                }
            }
            if !op.portsc(port_idx).ccs() {
                let is_usb3 = self.ports.get(port_idx).map(|p| p.is_usb3).unwrap_or(true);
                log::info!(
                    "xHCI: port {} no CCS after init_ports (portsc=0x{:08X} pls={}, usb3={})",
                    port_idx, op.portsc(port_idx).0, op.portsc(port_idx).pls(), is_usb3
                );
                // 1) Warm Port Reset (USB 3.0 only)
                // WPR re-initialises the USB 3.0 PHY and restarts link training.
                // Unlike PR, WPR is valid on ports in RxDetect with CCS=0.
                if is_usb3 {
                    let _ = warm_port_reset(op, port_idx);
                    if op.portsc(port_idx).ccs() {
                        log::info!("xHCI: port {} CCS=1 after WPR", port_idx);
                        self.log_portsc(port_idx);
                        continue;
                    }
                }

                // 2) Check port state after WPR (or directly for USB 2.0)
                let ps = op.portsc(port_idx);
                let in_rxdetect = ps.pls() == 5;

                if in_rxdetect {
                    // Port is in RxDetect (idle listening) — this is the correct
                    // state for an empty port.  PR on CCS=0+RxDetect may hang
                    // some controllers (PR never clears).  Skip it.
                    if is_usb3 {
                        log::info!("xHCI: port {} in RxDetect after WPR — idle, awaiting connection", port_idx);
                    } else {
                        log::info!("xHCI: port {} in RxDetect — idle, awaiting connection", port_idx);
                    }
                } else {
                    // Port is NOT in RxDetect — try Port Reset to recover.
                    // Linux only sets PR when CCS=1; on CCS=0 we only do PR
                    // when the port is in a non-idle state (e.g. Compliance,
                    // Polling stalled, Inactive) where a reset may help.
                    log::info!("xHCI: port {} CCS=0 in PLS={}, trying PR", port_idx, ps.pls());
                    let _ = port_reset(op, port_idx);
                    if op.portsc(port_idx).ccs() {
                        log::info!("xHCI: port {} CCS=1 after PR", port_idx);
                        self.log_portsc(port_idx);
                        continue;
                    }

                    // 3) Force U0 link state (valid for Polling/U3, NOT RxDetect)
                    let pls = op.portsc(port_idx).pls();
                    if pls != 5 {
                        log::info!("xHCI: port {} PR no CCS, U0 direct write (pls={})", port_idx, pls);
                        const PLS_U0: u32 = 0 << 5;
                        op.update_portsc(port_idx, PLS_U0 | PORTSC_LWS, PORTSC_PLS_MASK | PORTSC_LWS);
                        super::port::delay_ms(200);
                        if op.portsc(port_idx).ccs() {
                            log::info!("xHCI: port {} CCS=1 after U0 write", port_idx);
                        } else {
                            log::warn!("xHCI: port {} all recovery attempts failed", port_idx);
                        }
                    }
                }
                self.log_portsc(port_idx);
            }
        }

        // ── Diagnostic: dump final port states ──────────────────
        log::info!("xHCI: port initialisation complete, final states:");
        for port_idx in 0..self.ports.n_ports {
            self.log_portsc(port_idx);
        }
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
        let already_running = sts & USBSTS_HCH == 0;
        log::info!(
            "xHCI: USBCMD=0x{:08X} USBSTS=0x{:08X} HCHalted={} already_running={}",
            usbcmd, sts,
            (sts & USBSTS_HCH) != 0,
            already_running
        );

        log::info!("xHCI: performing HCRST");

        // Assert HCRST
        op.set_usbcmd(USBCMD_HCRST);

        // Intel-specific: 1ms delay after HCRST (Linux xhci_reset quirk)
        // Without this, register access may cause hangs on Intel controllers.
        super::port::delay_us(1000);

        for _ in 0..200_000 {
            if op.usbcmd() & USBCMD_HCRST == 0 {
                break;
            }
            super::port::delay_us(100);
        }

        // Wait for HCHalted
        for _ in 0..200_000 {
            if op.usbsts() & USBSTS_HCH != 0 {
                break;
            }
            super::port::delay_us(100);
        }

        // Wait for CNR (Controller Not Ready) to clear
        // The xHC needs time to initialize internal state after HCRST.
        for _ in 0..200_000 {
            if op.usbsts() & USBSTS_CNR == 0 {
                break;
            }
            super::port::delay_us(100);
        }

        let sts_after = op.usbsts();
        log::info!(
            "xHCI: after HCRST, USBSTS=0x{:08X} HCHalted={} CNR={}",
            sts_after, (sts_after & USBSTS_HCH) != 0, (sts_after & USBSTS_CNR) != 0
        );

        // Wait for CNR (Controller Not Ready) to clear after HCRST.
        // The xHC may need time to initialise its internal state
        // before accepting register writes (xHCI spec §5.4.2).
        for _ in 0..200_000 {
            if op.usbsts() & USBSTS_CNR == 0 {
                break;
            }
            super::port::delay_us(100);
        }
        if op.usbsts() & USBSTS_CNR != 0 {
            log::warn!("xHCI: CNR did not clear after HCRST");
            return Err("CNR timeout");
        }

        Ok(())
    }

    /// Configure DCBAA, CRCR, CONFIG, ERST, and interrupter registers.
    fn configure_registers(&mut self) -> Result<(), &'static str> {
        let op = &self.registers.op;
        let rt = &self.registers.runtime;

        // Set DCBAA
        op.set_dcbaap(self.device.dcbaa.phys);

        // Set CRCR (Command Ring Control)
        let crcr_val = self.rings.command.phys | 1; // RCS=1
        op.set_crcr(crcr_val);

        // Configure max slots
        op.set_config(self.device.slots.max_slots);

        // Allocate ERST page if not already allocated
        let ctx = self.driver_ctx;
        let erst_phys = if let Some(phys) = self.erst_phys {
            // Reuse existing ERST page
            phys
        } else {
            // Allocate new ERST page
            let phys = ctx
                .allocate_contiguous_frames(1)
                .map_err(|_| "no ERST page")?;
            self.erst_phys = Some(phys);
            phys
        };
        let erst_virt = ctx.phys_to_virt(erst_phys) as *mut ErstEntry;
        unsafe {
            ptr::write_volatile(erst_virt, ErstEntry::new(self.rings.event.phys, 256));
        }

        // Set ERST in runtime registers
        rt.set_erstsz(1);
        rt.set_erstba(erst_phys);

        // Set initial ERDP
        rt.set_erdp(self.rings.event.dequeue_ptr());

        // Enable primary interrupter (IMAN.IE) so the xHC activates
        // its event ring and port change machinery.  Some controllers
        // require this even when the driver polls USBSTS.PCD.
        self.interrupts.enable(rt);

        // Enable HSE (Host System Error) detection before RS=1.
        // The xHCI spec (§4.1) recommends HSEE=1 before starting
        // the controller.  Some implementations require this for
        // proper PHY operation.
        op.set_usbcmd_bits(USBCMD_HSEE);

        Ok(())
    }

    /// Start the controller — public for `HostController` trait.
    pub fn start(&mut self) -> Result<(), &'static str> {
        self.start_controller()
    }

    /// Internal RS=1 logic.
    fn start_controller(&mut self) -> Result<(), &'static str> {
        self.start_controller_no_inte()
    }

    /// Start the controller without enabling interrupts (RS | HSEE only).
    /// Used by init_no_reset() to start the controller before configure_registers().
    fn start_controller_no_inte(&mut self) -> Result<(), &'static str> {
        let op = &self.registers.op;

        op.set_usbcmd(USBCMD_RS | USBCMD_HSEE);
        for _ in 0..200_000 {
            if op.usbsts() & USBSTS_HCH == 0 {
                break;
            }
            super::port::delay_us(100);
        }

        if op.usbsts() & USBSTS_HCH != 0 {
            log::error!("xHCI: controller failed to start (HCHalted)");
            return Err("controller failed to start");
        }

        log::info!("xHCI: controller started");
        Ok(())
    }

    /// Enable interrupts after the event ring is configured.
    fn enable_interrupts(&mut self) {
        self.registers.op.set_usbcmd_bits(USBCMD_INTE);
    }

    /// Clear HSE and re-kick link training on all ports.
    pub fn clear_hse_and_recover(&mut self) {
        let op = &self.registers.op;
        let sts = op.usbsts();

        if sts & USBSTS_HSE == 0 {
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
        super::port::delay_ms(200);

        // WPR on any CCS=0 powered USB 3.0 ports, then wait for CCS
        for port_idx in 0..self.ports.n_ports {
            let ps = op.portsc(port_idx);
            let is_usb3 = self.ports.get(port_idx).map(|p| p.is_usb3).unwrap_or(true);
            if !ps.ccs() && ps.pp() && is_usb3 {
                if let Some(p) = self.ports.get_mut(port_idx) {
                    if !p.wpr_attempted {
                        p.wpr_attempted = true;
                        let _ = warm_port_reset(op, port_idx);
                        // Wait for link training to complete after WPR
                        for _ in 0..200 {
                            super::port::delay_ms(10);
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
        if op.usbsts() & USBSTS_PCD != 0 {
            op.clear_usbsts_bits(USBSTS_PCD);
            self.ports.clear_done_flags();
            log::info!("xHCI: PCD detected, re-evaluating all ports");
        }

        // ── Refresh all ports once before the loop ─────────
        // Previously refresh_all was called inside the per-port loop,
        // which caused N full PORTSC scans (one per port) and could
        // overwrite the local state of the port currently being processed.
        self.ports.refresh_all(op);

        for port_idx in 0..self.ports.n_ports {
            // Skip already-processed ports
            let skip = self.ports.get(port_idx).map(|p| p.done).unwrap_or(true);
            if skip {
                continue;
            }

            // ── Power-cycle if PPC supported ──────────────
            let needs_power_cycle = self
                .ports
                .get(port_idx)
                .map(|p| self.ports.ppc && (!p.pp_on() || p.pls() == 5))
                .unwrap_or(false);
            if needs_power_cycle {
                power_cycle(op, port_idx);
            }

            let ps = op.portsc(port_idx);
            if !ps.ccs() {
                // ── Exit Compliance (PLS=15) if stuck ────
                exit_compliance(op, port_idx);

                // ── Warm Port Reset (USB 3.0 only) ───────
                let is_usb3 = self
                    .ports
                    .get(port_idx)
                    .map(|p| p.is_usb3)
                    .unwrap_or(true);
                let wpr_already = self
                    .ports
                    .get(port_idx)
                    .map(|p| p.wpr_attempted)
                    .unwrap_or(true);
                if is_usb3 && !wpr_already && ps.pp() {
                    log::info!(
                        "xHCI: port {} WPR start (portsc=0x{:08X} pls={})",
                        port_idx, ps.0, ps.pls()
                    );
                    if let Some(p) = self.ports.get_mut(port_idx) {
                        p.wpr_attempted = true;
                    }
                    let wpr_result = warm_port_reset(op, port_idx);
                    let _ps_after = op.portsc(port_idx);
                    // Wait for link training to complete after WPR
                    for _ in 0..120 {
                        super::port::delay_ms(10);
                        if op.portsc(port_idx).ccs() {
                            log::info!("xHCI: port {} CCS=1 after WPR", port_idx);
                            break;
                        }
                    }
                    let wpr_val = wpr_result.map(|ps| ps.0).unwrap_or(0);
                    let ps_wpr = op.portsc(port_idx);
                    log::info!(
                        "xHCI: port {} WPR done wpr_val=0x{:08X} portsc=0x{:08X} ccs={} pls={}",
                        port_idx, wpr_val, ps_wpr.0, ps_wpr.ccs(), ps_wpr.pls()
                    );
                } else if !is_usb3 {
                    log::debug!("xHCI: port {} USB 2.0, skipping WPR", port_idx);
                }
            }

            let ps = op.portsc(port_idx);
            if !ps.ccs() {
                let is_usb3 = self.ports.get(port_idx).map(|p| p.is_usb3).unwrap_or(true);
                let wpr_already = self.ports.get(port_idx).map(|p| p.wpr_attempted).unwrap_or(true);
                let retry_cnt = self.ports.get(port_idx).map(|p| p.retry_count).unwrap_or(0);
                log::info!(
                    "xHCI: port {} CCS=0 → pls={} pp={} speed={} is_usb3={} wpr={} retry={}",
                    port_idx, ps.pls(), ps.pp() as u32, ps.speed(),
                    is_usb3, wpr_already, retry_cnt
                );
                // ── Force RxDetect to restart link training ─
                force_rx_detect(op, port_idx);
                super::port::delay_ms(100);
                let ps_rx = op.portsc(port_idx);
                log::info!(
                    "xHCI: port {} after RxDetect: portsc=0x{:08X} ccs={} pls={}",
                    port_idx, ps_rx.0, ps_rx.ccs(), ps_rx.pls()
                );
                if op.portsc(port_idx).ccs() {
                    // Link training succeeded — continue to port reset below
                } else {
                    // Check for HSE
                    if op.usbsts() & USBSTS_HSE != 0 {
                        op.clear_usbsts_bits(USBSTS_HSE);
                        force_rx_detect(op, port_idx);
                        super::port::delay_ms(200);
                        if op.portsc(port_idx).ccs() {
                            // Continue processing below
                        } else {
                            // HSE recovery didn't bring up CCS — increment retry
                            // count, mark done only after MAX_PORT_RETRIES.
                            if let Some(p) = self.ports.get_mut(port_idx) {
                                p.retry_count += 1;
                                if p.retry_count >= MAX_PORT_RETRIES {
                                    p.done = true;
                                }
                            }
                            continue;
                        }
                    } else {
                        // ── PP toggle fallback ─────────────
                        // Even when PPC=false, some controllers accept PP writes
                        // to force PHY re-initialisation.
                        let ps_raw = op.portsc(port_idx).0;
                        op.write_portsc(port_idx, (ps_raw & !PORTSC_RW1C_MASK) & !PORTSC_PP);
                        super::port::delay_ms(20);
                        let v2 = op.portsc(port_idx).0;
                        op.write_portsc(port_idx, (v2 & !PORTSC_RW1C_MASK) | PORTSC_PP);
                        super::port::delay_ms(50);
                        if op.portsc(port_idx).ccs() {
                            // Continue to port reset below
                        } else {
                            // ── Port Reset fallback ─────────
                            // Skip port_reset on ports that appear to be
                            // stably empty (PLS=RxDetect, no change bits).
                            let ps = op.portsc(port_idx);
                            let stable_empty = !ps.ccs() && ps.pls() == 5
                                && (ps.0 & PORTSC_RW1C_MASK) == 0;
                            if !stable_empty {
                                port_reset(op, port_idx).ok();
                            } else {
                                log::debug!("xHCI: port {} stably empty, skip reset", port_idx);
                            }
                            if op.portsc(port_idx).ccs() {
                                // Device appeared — continue to enable below
                            } else {
                                // Still no device — increment retry, mark done after max attempts
                                if let Some(p) = self.ports.get_mut(port_idx) {
                                    p.retry_count += 1;
                                    if p.retry_count >= MAX_PORT_RETRIES {
                                        p.done = true;
                                    }
                                }
                                log::debug!(
                                    "xHCI: port {} no device (ccs=0, pls={}, pp={}, retry={})",
                                    port_idx,
                                    op.portsc(port_idx).pls(),
                                    if op.portsc(port_idx).pp() { 1 } else { 0 },
                                    self.ports
                                        .get(port_idx)
                                        .map(|p| p.retry_count)
                                        .unwrap_or(0)
                                );
                                continue;
                            }
                        }
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
        let trb = Trb::new(trb_type::ENABLE_SLOT, self.rings.command.cycle);
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

        self.send_cmd(
            Trb::new(trb_type::ADDRESS_DEVICE, self.rings.command.cycle)
                .with_data_ptr(in_ctx_phys)
                .with_flags((slot_id << 24) | trb_flag::IOC)
        )?;

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

        self.send_cmd(
            Trb::new(trb_type::CONFIGURE_ENDPOINT, self.rings.command.cycle)
                .with_data_ptr(in_ctx_phys)
                .with_flags((slot_id << 24) | trb_flag::IOC)
        )?;

        Ok(())
    }

    /// Release a single device slot and free its resources.
    /// Sends a DISABLE_SLOT command per xHCI spec §4.6.5 before freeing.
    pub fn disable_slot(&mut self, slot_id: u32) {
        // Send DISABLE_SLOT command TRB (xHCI spec §6.4.3.8)
        let _ = self.send_cmd(
            Trb::new(trb_type::DISABLE_SLOT, self.rings.command.cycle)
                .with_flags(slot_id << 24)
        );
        let ctx = self.driver_ctx;
        self.device.dcbaa.clear_slot(slot_id);
        self.device.slots.release_slot(slot_id, ctx);
    }

    /// Release all device slots and free resources.
    /// Sends DISABLE_SLOT commands for each slot before freeing.
    pub fn disable_all_slots(&mut self) {
        let ctx = self.driver_ctx;
        let ids: Vec<u32> = self.device.slots.slots.iter().map(|s| s.slot_id).collect();
        for slot_id in &ids {
            let _ = self.send_cmd(
                Trb::new(trb_type::DISABLE_SLOT, self.rings.command.cycle)
                    .with_flags(*slot_id << 24)
            );
            self.device.dcbaa.clear_slot(*slot_id);
        }
        self.device.slots.release_all(ctx);
    }

    // ── Command submission ─────────────────────────────────────

    /// Enqueue a command TRB and wait for completion.
    /// Returns the event TRB flags on success, or an error if the
    /// event's completion code is not Success (xHCI spec §6.4.2.1).
    fn send_cmd(&mut self, trb: Trb) -> Result<u32, &'static str> {
        self.rings.command.enqueue(trb);
        self.registers.doorbell.ring(0, 0);
        let ev = wait_event(&mut self.rings.event, &self.registers.runtime, 5_000_000)?;
        if ev.completion_code() != COMP_SUCCESS {
            return Err("command completion code not success");
        }
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

        if let Some(slot) = self.device.slots.get_mut(slot_id) {
            // SETUP TRB (8-byte setup packet goes directly into params as Immediate Data)
            let trt = if data_len == 0 { 0 } else if is_in { 2 << 16 } else { 3 << 16 };
            let mut s_trb = Trb::new(trb_type::SETUP_STAGE, slot.ep0_ring.cycle);
            unsafe {
                core::ptr::copy_nonoverlapping(
                    setup as *const UsbSetupPacket as *const u8, s_trb.params.as_mut_ptr(), 8);
            }
            s_trb.flags |= trb_flag::CHAIN | trt;
            slot.ep0_ring.enqueue(s_trb);

            // DATA TRB (if any)
            if data_len > 0 {
                let dir = if is_in { trb_flag::DIR_IN } else { 0 };
                slot.ep0_ring.enqueue(
                    Trb::new(trb_type::DATA_STAGE, slot.ep0_ring.cycle)
                        .with_data_ptr(staging_phys)
                        .with_length(data_len as u32)
                        .with_flags(dir | trb_flag::CHAIN)
                );
            }

            // STATUS TRB
            let st_dir = if !is_in || data_len == 0 { trb_flag::DIR_IN } else { 0 };
            slot.ep0_ring.enqueue(
                Trb::new(trb_type::STATUS_STAGE, slot.ep0_ring.cycle)
                    .with_flags(st_dir | trb_flag::IOC)
            );
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

        // Free only when the transfer completed; timeout recovery must stop or
        // reset the endpoint before returning these pages to the allocator.
        if staging_phys != 0 && res.is_ok() {
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

            let dir_flag = if dir == UsbDirection::In { trb_flag::DIR_IN } else { 0 };
            ring.enqueue(
                Trb::new(trb_type::NORMAL, ring.cycle)
                    .with_data_ptr(staging_phys)
                    .with_length(len as u32)
                    .with_flags(dir_flag | trb_flag::IOC | trb_flag::ENT)
            );

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
        self.rings.command.free(self.driver_ctx);
        self.rings.event.free(self.driver_ctx);

        // Free DCBAA page
        let _ = self
            .driver_ctx
            .free_contiguous_frames(self.device.dcbaa.phys, 1);

        // Free ERST page
        if let Some(erst_phys) = self.erst_phys {
            let _ = self.driver_ctx.free_contiguous_frames(erst_phys, 1);
        }

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
    // Tests are in sub-modules ring, register, port.
}
