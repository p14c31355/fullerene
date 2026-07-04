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
use super::interrupt::*;
use super::port::*;
use super::register::*;
use super::ring::*;
use crate::usb::host_controller::HostController;

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
    /// ERST physical address (allocated in setup_erst).
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
        log::info!("xHCI: HCIVERSION=0x{:04X}", cap_regs.hci_version,);

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

    /// Initialise the controller: configure registers, start, init ports.
    ///
    /// Strategy:
    ///   Always do a full HCRST (Host Controller Reset) followed by proper
    ///   port power management.  The previous two-phase approach of trying
    ///   to preserve firmware state via `init_no_reset()` caused USB 3.0
    ///   PHY calibration loss on Wildcat Point-LP xHCI because stopping a
    ///   running controller disrupts the PHY and no amount of WPR/PR can
    ///   recover it afterwards.  A clean HCRST restores the controller to a
    ///   known state; following up with proper port power-cycling and link
    ///   training lets the hardware re-calibrate the PHY from scratch.
    ///
    /// This mirrors the Linux behaviour for Intel Wildcat Point-LP xHCI
    /// quirks (XHCI_RESET_ON_RESUME).
    pub fn init(&mut self) -> Result<(), &'static str> {
        let hci_ver = self.registers.cap.hci_version;
        log::info!("xHCI: hci_version=0x{:04X}", hci_ver);

        // Phase 1: Full HCRST.
        // Stop the controller first (it cannot be running during HCRST).
        let sts = self.registers.op.usbsts();
        if sts & USBSTS_HCH == 0 {
            log::info!("xHCI: controller running, halting before HCRST");
            self.registers
                .op
                .set_usbcmd(self.registers.op.usbcmd() & !USBCMD_RS);
            for _ in 0..200_000 {
                if self.registers.op.usbsts() & USBSTS_HCH != 0 {
                    break;
                }
                super::port::delay_us(100);
            }
            if self.registers.op.usbsts() & USBSTS_HCH == 0 {
                return Err("controller failed to halt before HCRST");
            }
        }

        self.controller_reset()?;
        self.configure_before_start();
        self.setup_erst()?;
        self.interrupts.enable(&self.registers.runtime);
        self.registers.op.set_usbcmd_bits(USBCMD_INTE);

        if self.registers.op.usbsts() & USBSTS_HSE != 0 {
            log::warn!("xHCI: HSE after HCRST, clearing");
            self.clear_hse_and_recover();
        }

        self.start_controller()?;
        self.clear_hse_and_recover();
        self.init_ports();

        Ok(())
    }

    /// Log PORTSC value for a single port with all relevant fields.
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

    /// Initialise all root-hub ports: ensure port power is on, kick
    /// link training via RxDetect, and wait for devices to appear
    /// (CCS=1).  This is called once after the controller starts.
    ///
    /// Recovery/power-cycle/RxDetect logic only runs for ports with CCS=0,
    /// preserving already-connected devices (CCS=1).
    fn init_ports(&mut self) {
        let op = &self.registers.op;
        log::info!("xHCI: initialising {} ports", self.ports.n_ports);

        // ── Acknowledge any stale change bits (WRC, PRC, etc.) ─
        // HCRST may leave WRC=1 (Warm Port Reset Change) on some
        // controllers.  If not acknowledged before other PORTSC writes,
        // the port may ignore link state transitions, preventing CCS.
        for port_idx in 0..self.ports.n_ports {
            let ps = op.portsc(port_idx).0;
            if ps & PORTSC_RW1C_MASK != 0 {
                op.write_portsc(port_idx, (ps & !PORTSC_RW1C_MASK) | (ps & PORTSC_RW1C_MASK));
            }
        }
        (0..self.ports.n_ports).for_each(|p| self.log_portsc(p));

        // ── Port power-up ─────────────────────────────────────
        // Power-cycle USB 3.0 ports that already have PP=1 (clean restart).
        // When PPC (Port Power Control) is supported, we can reliably control
        // port power.  When PPC is not supported, we still attempt a PP toggle
        // because some controllers (e.g. Intel Wildcat Point-LP) report PPC=0
        // in HCSPARAMS1 but still honor PP writes, and HCRST can leave USB 3.0
        // PHYs in a state where CCS never asserts without a power cycle.
        // IMPORTANT: Only power-cycle ports with CCS=0 to avoid disturbing
        // already-connected devices.
        for port_idx in 0..self.ports.n_ports {
            let ps = op.portsc(port_idx).0;
            let has_ccs = (ps & PORTSC_CCS) != 0;
            if has_ccs {
                continue;
            }
            let is_usb3 = self.ports.get(port_idx).map(|p| p.is_usb3).unwrap_or(true);
            if !is_usb3 {
                // USB 2.0 ports don't need PHY recalibration after HCRST
                continue;
            }
            if self.ports.ppc {
                if (ps & PORTSC_PP) != 0 {
                    op.write_portsc(port_idx, (ps & !PORTSC_RW1C_MASK) & !PORTSC_PP);
                    super::port::delay_ms(50);
                    op.write_portsc(
                        port_idx,
                        (op.portsc(port_idx).0 & !PORTSC_RW1C_MASK) | PORTSC_PP,
                    );
                    super::port::delay_ms(100);
                } else {
                    op.write_portsc(port_idx, (ps & !PORTSC_RW1C_MASK) | PORTSC_PP);
                    super::port::delay_ms(100);
                }
            } else {
                // PPC=0: PP may be read-only, but attempt toggle anyway for
                // controllers that honor it regardless of the capability bit.
                if (ps & PORTSC_PP) != 0 {
                    op.write_portsc(port_idx, (ps & !PORTSC_RW1C_MASK) & !PORTSC_PP);
                    super::port::delay_ms(50);
                    let v2 = op.portsc(port_idx).0;
                    op.write_portsc(port_idx, (v2 & !PORTSC_RW1C_MASK) | PORTSC_PP);
                    super::port::delay_ms(100);
                }
            }
        }
        (0..self.ports.n_ports).for_each(|p| self.log_portsc(p));

        // ── Exit Compliance mode + kick RxDetect ──────────────
        // Only perform recovery on ports with CCS=0 to avoid disturbing
        // already-connected devices.
        for port_idx in 0..self.ports.n_ports {
            let ps = op.portsc(port_idx);
            if ps.ccs() {
                continue;
            }
            super::port::exit_compliance(op, port_idx);
        }
        const PLS_RXDETECT: u32 = 5 << 5;
        // Only force RxDetect for USB 3.0 ports.  On USB 2.0 ports the
        // PLS field is not meaningful and writing LWS+PLS can leave the
        // controller's internal state machine in an undefined state.
        // USB 2.0 device detection is handled by the controller's own
        // hardware when PP=1 is set.
        for port_idx in 0..self.ports.n_ports {
            let ps = op.portsc(port_idx);
            if ps.ccs() {
                continue;
            }
            let is_usb3 = self.ports.get(port_idx).map(|p| p.is_usb3).unwrap_or(true);
            if is_usb3 {
                op.update_portsc(
                    port_idx,
                    PLS_RXDETECT | PORTSC_LWS,
                    PORTSC_PLS_MASK | PORTSC_LWS,
                );
            }
        }
        super::port::delay_ms(200);

        // ── Wait for link training (up to ~2s per port) ───────
        // Only wait for ports with CCS=0 (those we attempted to recover).
        for port_idx in 0..self.ports.n_ports {
            if op.portsc(port_idx).ccs() {
                log::info!("xHCI: port {} already has CCS=1", port_idx);
                continue;
            }
            let mut appeared = false;
            for _ in 0..200 {
                super::port::delay_ms(10);
                if op.portsc(port_idx).ccs() {
                    appeared = true;
                    break;
                }
            }
            if appeared {
                log::info!("xHCI: port {} CCS=1 after init_ports", port_idx);
                continue;
            }

            let is_usb3 = self.ports.get(port_idx).map(|p| p.is_usb3).unwrap_or(true);
            log::info!(
                "xHCI: port {} no CCS (portsc=0x{:08X} pls={} usb3={})",
                port_idx,
                op.portsc(port_idx).0,
                op.portsc(port_idx).pls(),
                is_usb3
            );

            // WPR for USB 3.0 restarts link training on idle ports.
            if is_usb3 {
                let _ = warm_port_reset(op, port_idx);
                if op.portsc(port_idx).ccs() {
                    log::info!("xHCI: port {} CCS=1 after WPR", port_idx);
                    self.log_portsc(port_idx);
                    continue;
                }
            }

            let pls = op.portsc(port_idx).pls();
            if pls == 15 {
                log::info!("xHCI: port {} still in Compliance, skip (idle)", port_idx);
            } else if pls == 5 {
                log::info!(
                    "xHCI: port {} in RxDetect — idle, awaiting connection",
                    port_idx
                );
            } else {
                log::info!("xHCI: port {} CCS=0 in PLS={}, trying PR", port_idx, pls);
                let _ = port_reset(op, port_idx);
                if op.portsc(port_idx).ccs() {
                    log::info!("xHCI: port {} CCS=1 after PR", port_idx);
                    continue;
                }
            }

            // PP toggle fallback: power-cycle the port to force PHY
            // re-initialization.  Some Intel xHCI controllers lose USB 3.0
            // PHY calibration after HCRST or stop/restart; toggling port
            // power forces the PHY to recalibrate.
            if self.ports.ppc && !op.portsc(port_idx).ccs() {
                log::info!("xHCI: port {} CCS=0, PP toggle fallback", port_idx);
                let ps_raw = op.portsc(port_idx).0;
                op.write_portsc(port_idx, (ps_raw & !PORTSC_RW1C_MASK) & !PORTSC_PP);
                super::port::delay_ms(50);
                let v2 = op.portsc(port_idx).0;
                op.write_portsc(port_idx, (v2 & !PORTSC_RW1C_MASK) | PORTSC_PP);
                super::port::delay_ms(200);

                // After PP toggle, force RxDetect and wait for training
                // Only for USB 3.0 ports; USB 2.0 doesn't use PLS and writing
                // LWS+PLS can leave the controller in an undefined state.
                let is_usb3 = self.ports.get(port_idx).map(|p| p.is_usb3).unwrap_or(true);
                if is_usb3 {
                    force_rx_detect(op, port_idx);
                }
                for _ in 0..200 {
                    super::port::delay_ms(10);
                    if op.portsc(port_idx).ccs() {
                        log::info!("xHCI: port {} CCS=1 after PP toggle", port_idx);
                        break;
                    }
                }
            }
            self.log_portsc(port_idx);
        }

        log::info!("xHCI: port initialisation complete");
        (0..self.ports.n_ports).for_each(|p| self.log_portsc(p));
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
            usbcmd,
            sts,
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
        if op.usbcmd() & USBCMD_HCRST != 0 {
            log::warn!("xHCI: HCRST did not clear");
            return Err("HCRST timeout");
        }

        // Wait for HCHalted
        for _ in 0..200_000 {
            if op.usbsts() & USBSTS_HCH != 0 {
                break;
            }
            super::port::delay_us(100);
        }
        if op.usbsts() & USBSTS_HCH == 0 {
            log::warn!("xHCI: controller did not halt after HCRST");
            return Err("HCHalted timeout");
        }

        // Wait for CNR (Controller Not Ready) to clear
        // The xHC needs time to initialise its internal state
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

        let sts_after = op.usbsts();
        log::info!(
            "xHCI: after HCRST, USBSTS=0x{:08X} HCHalted={} CNR={}",
            sts_after,
            (sts_after & USBSTS_HCH) != 0,
            (sts_after & USBSTS_CNR) != 0
        );

        Ok(())
    }

    /// Configure core registers that must be written before RS=1:
    /// DCBAAP, CRCR, CONFIG, HSEE.
    fn configure_before_start(&mut self) {
        let op = &self.registers.op;
        op.set_dcbaap(self.device.dcbaa.phys);
        op.set_crcr(self.rings.command.phys | 1);
        op.set_config(self.device.slots.max_slots);
        op.set_usbcmd_bits(USBCMD_HSEE);
    }

    /// Allocate ERST (if needed) and configure runtime registers:
    /// ERSTSZ, ERSTBA, ERDP.
    fn setup_erst(&mut self) -> Result<(), &'static str> {
        let rt = &self.registers.runtime;
        let ctx = self.driver_ctx;
        let erst_phys = if let Some(phys) = self.erst_phys {
            phys
        } else {
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
        rt.set_erstsz(1);
        rt.set_erstba(erst_phys);
        rt.set_erdp(self.rings.event.dequeue_ptr());
        Ok(())
    }

    /// Start the controller — public for `HostController` trait.
    pub fn start(&mut self) -> Result<(), &'static str> {
        self.start_controller()
    }

    /// Start the controller (RS | HSEE) and wait for HCHalted to clear.
    fn start_controller(&mut self) -> Result<(), &'static str> {
        let op = &self.registers.op;

        op.set_usbcmd_bits(USBCMD_RS | USBCMD_HSEE);
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

    /// Clear HSE and re-kick link training on all ports.
    pub fn clear_hse_and_recover(&mut self) {
        let op = &self.registers.op;
        let sts = op.usbsts();

        if sts & USBSTS_HSE == 0 {
            return;
        }

        log::info!("xHCI: HSE detected, recovering...");
        op.clear_usbsts_bits(USBSTS_HSE);

        // Force RxDetect on all USB 3.0 ports only.  Writing LWS+PLS to
        // USB 2.0 ports is undefined per the xHCI spec and may leave the
        // controller's internal state machine in an undefined state.
        for port in 0..self.ports.n_ports {
            let is_usb3 = self.ports.get(port).map(|p| p.is_usb3).unwrap_or(true);
            if is_usb3 {
                force_rx_detect(op, port);
            }
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
        let mut added = 0usize;

        // PCD (Port Change Detect) → re-evaluate changed ports
        if self.registers.op.usbsts() & USBSTS_PCD != 0 {
            self.registers.op.clear_usbsts_bits(USBSTS_PCD);
            // Save pre-refresh connected state per port
            let pre_ccs: alloc::vec::Vec<bool> = (0..self.ports.n_ports)
                .map(|i| self.ports.get(i).map(|p| p.ccs()).unwrap_or(false))
                .collect();
            self.ports.refresh_all(&self.registers.op);
            // Only clear done for ports whose CCS changed
            for port_idx in 0..self.ports.n_ports {
                let ccs = self.ports.get(port_idx).map(|p| p.ccs()).unwrap_or(false);
                let was = pre_ccs.get(port_idx as usize).copied().unwrap_or(false);
                if ccs != was {
                    if let Some(p) = self.ports.get_mut(port_idx) {
                        p.done = false;
                        p.wpr_attempted = false;
                        p.retry_count = 0;
                        log::info!(
                            "xHCI: port {} CCS changed ({} → {}), re-evaluating",
                            port_idx,
                            was,
                            ccs
                        );
                    }
                    // When CCS transitions 0→1 or 1→0, remove stale device
                    // entry for this specific port only.
                    self.devices.retain(|d| d.port_index != port_idx);
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
                // CCS became 0 → device was disconnected; remove entry for this port only
                if !self.registers.op.portsc(port_idx).ccs() {
                    self.devices.retain(|d| d.port_index != port_idx);
                    log::info!("xHCI: port {} disconnected", port_idx);
                }
                continue;
            }

            // Device confirmed — record it
            let ps = self.registers.op.portsc(port_idx);
            let speed = port_speed_to_usb(ps.speed());
            log::info!("xHCI: port {} device detected, speed={:?}", port_idx, speed);

            // Remove any stale device record for this port before adding a new one
            self.devices.retain(|d| d.port_index != port_idx);

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
            if let Some(p) = self.ports.get_mut(port_idx) {
                p.done = true;
            }
        }

        added
    }

    /// Try to bring up a single port.  Returns true if CCS=1 and PED=1.
    fn try_connect_port(&mut self, port_idx: u32) -> bool {
        let op = &self.registers.op;

        // Refresh port state
        if let Some(p) = self.ports.get_mut(port_idx) {
            p.refresh(op);
        }

        // Power-cycle when PPC is supported and the port lacks power
        if self.ports.ppc {
            if let Some(p) = self.ports.get(port_idx) {
                if !p.pp_on() {
                    power_cycle(op, port_idx);
                }
            }
        }

        let ps = op.portsc(port_idx);
        if ps.ccs() && ps.ped() {
            return true;
        }

        // ── Phase 1: Exit Compliance / Warm Port Reset / RxDetect ──
        if !ps.ccs() {
            exit_compliance(op, port_idx);
            let ps = op.portsc(port_idx);
            if ps.ccs() && ps.ped() {
                return true;
            }

            let is_usb3 = self.ports.get(port_idx).map(|p| p.is_usb3).unwrap_or(true);
            let wpr_done = self
                .ports
                .get(port_idx)
                .map(|p| p.wpr_attempted)
                .unwrap_or(true);

            if is_usb3 && !wpr_done && ps.pp() {
                if let Some(p) = self.ports.get_mut(port_idx) {
                    p.wpr_attempted = true;
                }
                log::info!(
                    "xHCI: port {} WPR (portsc=0x{:08X} pls={})",
                    port_idx,
                    ps.0,
                    ps.pls()
                );
                let _ = warm_port_reset(op, port_idx);
                for _ in 0..120 {
                    super::port::delay_ms(10);
                    if op.portsc(port_idx).ccs() {
                        log::info!("xHCI: port {} CCS=1 after WPR", port_idx);
                        break;
                    }
                }
                if op.portsc(port_idx).ccs() && op.portsc(port_idx).ped() {
                    return true;
                }
            }

            // Force RxDetect to restart link training (USB 3.0 only).
            // For USB 2.0 ports, the PLS field is not meaningful and writing
            // LWS+PLS can leave the controller's internal state machine in
            // an undefined state.  USB 2.0 detection relies on PP=1 and PR.
            if !op.portsc(port_idx).ccs() {
                if is_usb3 {
                    force_rx_detect(op, port_idx);
                    super::port::delay_ms(200);
                } else {
                    // For USB 2.0 ports, just wait for the device to assert
                    // CCS after power-up.  The controller's hardware handles
                    // this automatically.
                    for _ in 0..200 {
                        super::port::delay_ms(10);
                        if op.portsc(port_idx).ccs() {
                            break;
                        }
                    }
                }

                if op.portsc(port_idx).ccs() && op.portsc(port_idx).ped() {
                    return true;
                }

                // HSE recovery
                if op.usbsts() & USBSTS_HSE != 0 {
                    op.clear_usbsts_bits(USBSTS_HSE);
                    if is_usb3 {
                        force_rx_detect(op, port_idx);
                    }
                    super::port::delay_ms(300);
                    if op.portsc(port_idx).ccs() && op.portsc(port_idx).ped() {
                        return true;
                    }
                }

                // PP toggle fallback
                let ps_raw = op.portsc(port_idx).0;
                op.write_portsc(port_idx, (ps_raw & !PORTSC_RW1C_MASK) & !PORTSC_PP);
                super::port::delay_ms(50);
                let v2 = op.portsc(port_idx).0;
                op.write_portsc(port_idx, (v2 & !PORTSC_RW1C_MASK) | PORTSC_PP);
                super::port::delay_ms(200);

                if op.portsc(port_idx).ccs() && op.portsc(port_idx).ped() {
                    return true;
                }
            }
        }

        // ── Phase 2: Port Reset (PED=0 but CCS=1, or recovery from bad state) ──
        let ps = op.portsc(port_idx);
        if ps.ccs() && !ps.ped() {
            let _ = port_reset(op, port_idx);
            if op.portsc(port_idx).ccs() && op.portsc(port_idx).ped() {
                return true;
            }
        }

        // ── Phase 3: No device — retry management ──
        if let Some(p) = self.ports.get_mut(port_idx) {
            p.retry_count = p.retry_count.saturating_add(1);
            if p.retry_count >= MAX_PORT_RETRIES {
                p.done = true;
                log::debug!(
                    "xHCI: port {} done after {} retries",
                    port_idx,
                    p.retry_count
                );
            } else {
                log::debug!(
                    "xHCI: port {} no device (ccs={} pls={} pp={} retry={})",
                    port_idx,
                    op.portsc(port_idx).ccs() as u32,
                    op.portsc(port_idx).pls(),
                    op.portsc(port_idx).pp() as u32,
                    p.retry_count,
                );
            }
        }
        false
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
                .with_flags((slot_id << 24) | trb_flag::IOC),
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

        let cmd = self.send_cmd(
            Trb::new(trb_type::CONFIGURE_ENDPOINT, self.rings.command.cycle)
                .with_data_ptr(in_ctx_phys)
                .with_flags((slot_id << 24) | trb_flag::IOC),
        );
        if cmd.is_err() {
            bulk_ring.free(self.driver_ctx);
            return cmd.map(|_| ());
        }

        if let Some(slot) = self.device.slots.get_mut(slot_id) {
            if is_in {
                slot.bulk_in_ring = Some(bulk_ring);
            } else {
                slot.bulk_out_ring = Some(bulk_ring);
            }
        }

        Ok(())
    }

    /// Release a single device slot and free its resources.
    /// Sends a DISABLE_SLOT command per xHCI spec §4.6.5 before freeing.
    pub fn disable_slot(&mut self, slot_id: u32) {
        // Send DISABLE_SLOT command TRB (xHCI spec §6.4.3.8)
        let _ = self.send_cmd(
            Trb::new(trb_type::DISABLE_SLOT, self.rings.command.cycle).with_flags(slot_id << 24),
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
                    .with_flags(*slot_id << 24),
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
        // Write barrier: ensure enqueued TRB is visible to the xHC
        // via DMA before ringing the doorbell (MMIO).  Without this,
        // the xHC may read stale TRB data from cache.
        crate::mmio::write_barrier();
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
            let trt = if data_len == 0 {
                0
            } else if is_in {
                2 << 16
            } else {
                3 << 16
            };
            let mut s_trb = Trb::new(trb_type::SETUP_STAGE, slot.ep0_ring.cycle)
                .with_length(8)
                .with_flags(trb_flag::IDT);
            unsafe {
                core::ptr::copy_nonoverlapping(
                    setup as *const UsbSetupPacket as *const u8,
                    s_trb.params.as_mut_ptr(),
                    8,
                );
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
                        .with_flags(dir | trb_flag::CHAIN),
                );
            }

            // STATUS TRB
            let st_dir = if !is_in || data_len == 0 {
                trb_flag::DIR_IN
            } else {
                0
            };
            slot.ep0_ring.enqueue(
                Trb::new(trb_type::STATUS_STAGE, slot.ep0_ring.cycle)
                    .with_flags(st_dir | trb_flag::IOC),
            );
        }

        // Ensure all EP0 TRB writes are visible to the xHC before doorbell
        crate::mmio::write_barrier();
        // Doorbell EP0 (DCI=1)
        self.registers.doorbell.ring(slot_id, 1);
        let res = self.wait_event(5_000_000);

        // Copy IN data back
        if res.is_ok() && is_in && data_len > 0 {
            unsafe {
                ptr::copy_nonoverlapping(staging_virt, buf.as_mut_ptr(), data_len);
            }
        }

        // Free staging buffer unconditionally.  On timeout, the endpoint
        // must be reset/recovered before the next use, but the pages must
        // not leak regardless.  The caller is responsible for recovery.
        if staging_phys != 0 {
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

            let dir_flag = if dir == UsbDirection::In {
                trb_flag::DIR_IN
            } else {
                0
            };
            ring.enqueue(
                Trb::new(trb_type::NORMAL, ring.cycle)
                    .with_data_ptr(staging_phys)
                    .with_length(len as u32)
                    .with_flags(dir_flag | trb_flag::IOC | trb_flag::ENT),
            );

            let ep_num = (endpoint & 0x0F) as u32;
            let is_in = (endpoint & 0x80) != 0;
            ep_num * 2 + u32::from(is_in)
        };

        // Ensure all bulk TRB writes are visible to the xHC before doorbell
        crate::mmio::write_barrier();
        self.registers.doorbell.ring(slot_id, db_stream);
        let res = self.wait_event(5_000_000);

        // Validate transfer event and copy IN data back; free staging buffer
        let actual_len = match res {
            Ok(ev) => {
                if ev.completion_code() != COMP_SUCCESS {
                    self.driver_ctx
                        .free_contiguous_frames(staging_phys, staging_pages);
                    return Err("bulk completion code not success");
                }
                let remainder = ev.remaining() as usize;
                let xfer_len = len.saturating_sub(remainder.min(len));
                if dir == UsbDirection::In && xfer_len > 0 {
                    unsafe {
                        ptr::copy_nonoverlapping(staging_virt, buf.as_mut_ptr(), xfer_len);
                    }
                }
                self.driver_ctx
                    .free_contiguous_frames(staging_phys, staging_pages);
                Ok(xfer_len)
            }
            Err(_) => {
                self.driver_ctx
                    .free_contiguous_frames(staging_phys, staging_pages);
                Err("bulk transfer failed")
            }
        };

        actual_len
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
