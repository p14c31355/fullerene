//! DWC3 device reset, Run/Stop, and controller halt-state handling.

use super::super::usb_regs::*;
use super::config::run_stop_value;
use super::log::{log_hex, log_puts};
use super::mmio::*;
use super::trace::{
    TRACE_DEVICE_RESET, TRACE_DWC3_HALT_TIMEOUT, TRACE_DWC3_HALTED, TRACE_DWC3_RESET_BEGIN,
    trace_event,
};

/// Reset the DWC3 core after taking ownership from the bootloader.
///
/// The Qualcomm glue invokes this as part of the DWC3 post-reset path. A
/// `fastboot boot` handoff skips that driver, so leaving the controller in its
/// bootloader device/host state can make endpoint commands retire without
/// ever allowing the peripheral pull-up to become visible.
pub(super) unsafe fn device_soft_reset() -> bool {
    unsafe {
        trace_event(TRACE_DWC3_RESET_BEGIN, 0, 0, 0, 0, 0);
        trace_event(TRACE_DEVICE_RESET, 0, 0, 0, 0, 0);
        let initial_dctl = read(DCTL);
        let source_exact = cfg!(fullerene_aarch64_usb_usb2_source_exact_device_reset);
        if source_exact {
            // qpr1's dwc3_device_core_soft_reset() writes DCTL verbatim
            // after adding CSFTRST. In particular, it does not clear
            // RUN_STOP or mask TRGTULST before the reset.
            write(DCTL, initial_dctl | DCTL_CSFTRST);
        } else {
            // Match Linux's reconnect path: clear stale endpoint/device state
            // without touching the already-running Qualcomm PHY and clock
            // branches. RUN_STOP must be cleared in the same write; preserving
            // Fastboot's RUN_STOP bit can leave the device half-running while
            // CSFTRST is asserted.
            let mut dctl = initial_dctl;
            dctl |= DCTL_CSFTRST;
            dctl &= !DCTL_RUN_STOP;
            write_dctl_safe(dctl);
        }
        let snpsid = read(GSNPSID);
        let ip = snpsid >> 16;
        // DWC_usb31 1.90a+ and DWC_usb32 synchronize CSFTRST through all
        // clocks and need the slower 20-ms polling cadence used by Linux.
        // Bramble's 0x5533 controller follows the ordinary 1-us path.
        let version = if ip == DWC31_IP || ip == DWC32_IP {
            read(VER_NUMBER)
        } else {
            0
        };
        let slow_reset = ip == DWC32_IP || (ip == DWC31_IP && version >= DWC31_REVISION_190A);
        let retries = if source_exact || slow_reset {
            10
        } else {
            1_000
        };
        let mut device_reset_complete = false;
        for _ in 0..retries {
            if read(DCTL) & DCTL_CSFTRST == 0 {
                device_reset_complete = true;
                break;
            }
            if source_exact {
                crate::timer::delay_ms(1);
            } else if slow_reset {
                crate::timer::delay_ms(20);
            } else {
                crate::timer::delay_us(1);
            }
        }
        if !device_reset_complete {
            log_puts("usb: DWC3 device reset timeout\n");
            return false;
        }

        // qpr1 uses a 1-ms usleep cadence for this device-core reset. The
        // source-exact A/B keeps that cadence; the existing path retains its
        // controller-revision-specific polling delay.
        // DWC_usb31 requires a synchronization delay after CSFTRST clears
        // before software accesses the PHY domain. The Bramble 4.19 driver
        // applies this to every DWC_usb31 revision, not only the older
        // 1.80a-and-earlier parts; the reset completion poll alone is not
        // sufficient for the PHY clock-domain crossing to settle.
        if ip == DWC31_IP {
            crate::timer::delay_ms(50);
        }
        if source_exact {
            // Android's source-exact reset clears the DWC3 doorbell block
            // immediately after the 50-ms PHY synchronization delay.
            super::clear_gsi_doorbell_state();
        } else {
            #[cfg(fullerene_aarch64_usb_gadget_handoff_clear_gsi_after_reset)]
            {
                // Keep the source-confirmed Qualcomm GSI doorbell transition
                // isolated from the DWC3 reset and endpoint/PHY A/Bs.
                super::clear_gsi_doorbell_state();
            }
        }
        true
    }
}

/// Mirror Linux's dwc3_gadget_dctl_write_safe(). DCTL's link-state request
/// field is a command, not persistent configuration; carrying a Fastboot
/// request into CSFTRST or Run/Stop can make the next device transition race
/// the controller's link state machine.
#[inline]
pub(super) unsafe fn write_dctl_safe(value: u32) {
    unsafe { write(DCTL, value & !DCTL_TRGTULST_MASK) };
}

/// Reset the DWC3 core and both PHY-facing domains for a cold platform start.
///
/// This is intentionally separate from `device_soft_reset`: a Fastboot
/// handoff must not reset the PHYs that own the Type-C session.
pub(super) unsafe fn core_soft_reset(super_speed: bool) -> bool {
    unsafe {
        if !device_soft_reset() {
            return false;
        }

        // The DWC_usb31 reference reset sequence is DCTL.CSFTRST after the
        // external PHY reset. A second GCTL.CORESOFTRESET is not part of
        // that sequence and can invalidate the device-side state that was
        // just synchronized. Keep this as a hardware A/B so DWC3 revisions
        // retain the existing full-core reset path.
        if cfg!(fullerene_aarch64_usb_dwc31_dctl_only_reset) && read(GSNPSID) >> 16 == DWC31_IP {
            return true;
        }

        // B3a: the CSFTRST handshake completed (DCTL.CSFTRST cleared) but
        // the GCTL.CORESOFTRESET / PHY_PHYSOFTRST section has not started.
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        super::init_beacon();

        let mut gctl = read(GCTL);
        gctl |= GCTL_CORESOFTRESET;
        write(GCTL, gctl);

        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 |= GUSB2PHYCFG_PHYSOFTRST;
        write(GUSB2PHYCFG0, usb2);
        if super_speed {
            let mut usb3 = read(GUSB3PIPECTL0);
            usb3 |= GUSB3PIPECTL_PHYSOFTRST;
            write(GUSB3PIPECTL0, usb3);
        }

        // The upstream DWC3 core reset uses a 100 ms delay after releasing
        // both PHY resets. The architectural counter is firmware-provided
        // before this early probe, so use it instead of a CPU-dependent loop.
        crate::timer::delay_ms(100);

        usb2 = read(GUSB2PHYCFG0) & !GUSB2PHYCFG_PHYSOFTRST;
        write(GUSB2PHYCFG0, usb2);
        if super_speed {
            let mut usb3 = read(GUSB3PIPECTL0);
            usb3 &= !GUSB3PIPECTL_PHYSOFTRST;
            write(GUSB3PIPECTL0, usb3);
        }
        crate::timer::delay_ms(1);

        gctl = read(GCTL) & !GCTL_CORESOFTRESET;
        write(GCTL, gctl);
        true
    }
}

/// Stop a controller that was left running by Fastboot before reusing its
/// device-mode endpoint state. A DWC3 gadget must be halted before
/// DEPSTARTCFG/SETEPCONFIG are issued; a handoff cannot assume that the
/// bootloader performed the normal gadget-stop sequence.
pub(super) unsafe fn stop_running_device() -> bool {
    #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_disable_gadget_irq_before_stop)]
    {
        // Linux disables DWC3 gadget event interrupts at the start of the
        // official teardown. Keep this source-confirmed write separate from
        // endpoint cleanup and the Run/Stop transition.
        write(DEVTEN, 0);
        let _ = read(DEVTEN);
    }
    #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_disable_ep0_before_stop)]
    {
        // The official teardown invokes __dwc3_gadget_ep_disable() for EP0
        // OUT and IN, which clears their DALEPENA bits. Keep that hardware
        // write separate from active-transfer cleanup and Run/Stop.
        let dalepena = read(DALEPENA);
        write(DALEPENA, dalepena & !0b11);
        let _ = read(DALEPENA);
    }
    unsafe { run_stop_device(false) }
}

const DWC3_RUN_STOP_POLL_MS: u64 = 1;
const DWC3_RUN_STOP_TIMEOUT_MS: u64 = 2_000;

/// Wait for DWC3's device controller to reach the requested halt state after
/// a Run/Stop write. Linux polls DSTS at 1--2 ms intervals for up to 2,000
/// iterations; a fixed NOP count is too short on a fast boot CPU and can let
/// endpoint commands race the controller's previous Fastboot session.
pub(super) unsafe fn wait_device_state(want_halted: bool) -> bool {
    unsafe {
        for _ in 0..DWC3_RUN_STOP_TIMEOUT_MS {
            // The DWC3 databook requires software to acknowledge device
            // events while waiting for DEVCTRLHLT during a gadget stop.
            // Linux's soft-disconnect path does this in parallel with the
            // halt poll; a Fastboot-owned stale disconnect/reset event can
            // otherwise keep the controller from completing Run/Stop.
            if want_halted {
                acknowledge_events_while_halting();
            }
            let dsts = read(DSTS);
            let halted = dsts & DSTS_DEVCTRLHLT != 0;
            if halted == want_halted {
                if want_halted {
                    trace_event(TRACE_DWC3_HALTED, 0, 0, 0, 0, dsts);
                }
                return true;
            }
            crate::timer::delay_ms(DWC3_RUN_STOP_POLL_MS);
        }
        let dsts = read(DSTS);
        trace_event(TRACE_DWC3_HALT_TIMEOUT, want_halted as u32, 0, 0, 0, dsts);
        log_hex(
            if want_halted {
                "usb: DWC3 stop timeout during handoff, DSTS="
            } else {
                "usb: DWC3 start timeout during handoff, DSTS="
            },
            dsts as u64,
        );
        false
    }
}

/// Acknowledge events generated while DWC3 is draining a device stop.
///
/// This is intentionally separate from `acknowledge_ep0_event_count()`: event
/// buffer setup initializes the complete GEVNTCOUNT register with zero, while
/// the Run/Stop halt contract consumes only the byte count and advances the
/// software cursor just as Linux advances `ev_buf->lpos`.
pub(super) unsafe fn acknowledge_events_while_halting() {
    unsafe {
        let count = read(GEVNTCOUNT0) & GEVNTCOUNT_MASK;
        if count != 0 {
            write(GEVNTCOUNT0, count);
            super::EVENT_OFFSET = (super::EVENT_OFFSET + count as usize) % super::ep0_event_size();
            core::arch::asm!("dsb sy", options(nostack));
        }
    }
}

/// Release the DWC3 USB3 PIPE soft-reset bit without issuing a core reset.
///
/// The normal `dwc3_core_soft_reset()` path clears this bit as part of its
/// core/PHY reset sequence. A `--no-core-reset` handoff deliberately skips
/// that sequence, but it still reinitializes the external QMP PHY. Keep this
/// as an opt-in differential: clearing an already-clear bit is harmless, and
/// the A/B can test whether Fastboot left the DWC3-side PIPE held in reset
/// after the QMP ownership transition.
#[inline]
pub(super) unsafe fn release_usb3_phy_reset() {
    unsafe {
        let mut usb3 = read(GUSB3PIPECTL0);
        usb3 &= !GUSB3PIPECTL_PHYSOFTRST;
        write(GUSB3PIPECTL0, usb3);
        let _ = read(GUSB3PIPECTL0);
    }
}

/// Apply the shared Linux-style preparation for a DWC3 Run/Stop transition.
/// The caller restores the saved USB2 low-power bits after any required halt
/// wait, so both the checked and no-readback paths launch the same transition.
unsafe fn prepare_run_stop_device(is_on: bool) -> u32 {
    unsafe {
        #[cfg(any(
            fullerene_aarch64_usb_gadget_handoff_no_usb2_runstop_guard,
            fullerene_aarch64_usb_gadget_handoff_usb2_source_exact_runstop
        ))]
        if is_on {
            // The Bramble downstream dwc3-gadget run/stop path does not use
            // mainline's temporary SUSPHY/ENBLSLPM clear. Keep this narrow
            // A/B for the direct USB2 handoff so the DCTL transition can be
            // compared with the vendor source without changing the normal
            // Linux-compatible path.
            let mut dctl = read(DCTL);
            if cfg!(any(
                fullerene_aarch64_usb_gadget_handoff_source_exact_runstop,
                fullerene_aarch64_usb_gadget_handoff_usb2_source_exact_runstop
            )) && matches!(read(GSNPSID) >> 16, DWC31_IP | DWC32_IP)
            {
                // The source-exact USB2 variant returns through this early
                // path, so it needs the same qpr1 revision-gated
                // KEEP_CONNECT clear as the checked path below.
                dctl &= !DCTL_KEEP_CONNECT;
            }
            write(
                DCTL,
                if cfg!(any(
                    fullerene_aarch64_usb_gadget_handoff_source_exact_runstop,
                    fullerene_aarch64_usb_gadget_handoff_usb2_source_exact_runstop
                )) {
                    dctl | DCTL_RUN_STOP
                } else {
                    run_stop_value(dctl, read(GSNPSID))
                },
            );
            return 0;
        }
        let mut usb2 = read(GUSB2PHYCFG0);
        let saved_config = usb2 & (GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        if saved_config != 0 {
            usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
            write(GUSB2PHYCFG0, usb2);
        }

        let mut dctl = read(DCTL);
        if cfg!(any(
            fullerene_aarch64_usb_gadget_handoff_source_exact_runstop,
            fullerene_aarch64_usb_gadget_handoff_usb2_source_exact_runstop
        )) {
            // Android msm's gadget_run_stop() re-reads DCTL after gadget
            // start and applies the source-required Run/Stop bit without
            // rewriting HIRD/APPL1RES. The qpr1 revision-gated
            // KEEP_CONNECT clear is retained below for DWC_usb31. Keep this
            // A/B narrow so the immediate readback can distinguish a
            // rejected bit write from a local policy rewrite.
            if is_on {
                // qpr1's dwc3_gadget_run_stop() clears KEEP_CONNECT on
                // revisions >= 1.94a before __dwc3_gadget_start(). For a
                // DWC_usb31 core the vendor driver represents VER_NUMBER
                // with DWC3_REVISION_IS_DWC31, so Bramble's 0x3331 core is
                // in that revision-gated branch as well. Preserve the
                // source-exact policy while changing no other DCTL fields.
                if matches!(read(GSNPSID) >> 16, DWC31_IP | DWC32_IP) {
                    dctl &= !DCTL_KEEP_CONNECT;
                }
                dctl |= DCTL_RUN_STOP;
            } else {
                dctl &= !DCTL_RUN_STOP;
            }
            write(DCTL, dctl);
        } else if cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_raw_runstop) {
            // Stock XBL's DwcRunStop only changes DCTL.RUN_STOP. Its core-init
            // sequence has already installed HIRD=7/APPL1RES, and it
            // preserves KEEP_CONNECT/TRGTULST instead of applying Linux's
            // generic reconnect policy here.
            if is_on {
                dctl = (dctl & !DCTL_HIRD_THRES_MASK) | DCTL_HIRD_THRES_XBL;
                dctl |= DCTL_APPL1RES | DCTL_RUN_STOP;
            } else {
                dctl &= !DCTL_RUN_STOP;
            }
            write(DCTL, dctl);
        } else if is_on {
            dctl = run_stop_value(dctl, read(GSNPSID));
            // `run_stop_value` deliberately preserves RX_DET for older DWC3
            // revisions. `write_dctl_safe` is used by reset/stop paths and
            // clears that target bit, so the Run/Stop transition must write
            // this value directly.
            write(DCTL, dctl);
        } else {
            dctl &= !DCTL_RUN_STOP;
            #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_clear_keep_connect_before_stop)]
            if read(GHWPARAMS1) & GHWPARAMS1_EN_PWROPT_MASK == GHWPARAMS1_EN_PWROPT_HIB {
                // Linux clears KEEP_CONNECT on a non-suspend gadget stop
                // when the DWC3 core advertises hibernation. Keep this
                // source-confirmed A/B in the same DCTL write as RUN_STOP.
                dctl &= !DCTL_KEEP_CONNECT;
            }
            write_dctl_safe(dctl);
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_clear_gsi_stop_state)]
        if !is_on {
            // The official stop path clears the GSI event-buffer counts and
            // Qualcomm doorbell wrapper immediately after DCTL.RUN_STOP is
            // cleared, before waiting for DEVCTRLHLT.
            super::clear_gsi_stop_state();
        }
        // Diagnostic only: put the USB2 interface contract back immediately
        // after the DCTL Run/Stop write, before the controller's state
        // transition completes. This distinguishes a write rejected while
        // running from a transition-time reset/overwrite of GUSB2PHYCFG.
        if is_on && option_env!("FULLERENE_USB_UTMI_WRITE_AFTER_DCTL") == Some("1") {
            super::config::configure_usb2_phy_interface();
        }
        if is_on && option_env!("FULLERENE_USB_DALEPENA_AFTER_DCTL") == Some("1") {
            // Diagnostic A/B: if Run/Stop itself drops the endpoint-enable
            // mask, restore only EP0 OUT/IN here and let the gate-time
            // snapshot report whether the mask survives the later reset/link
            // transition. No PHY or transfer descriptor is changed.
            write(DALEPENA, 0b11);
        }
        if is_on && option_env!("FULLERENE_USB_DALEPENA_AFTER_DCTL") == Some("1") {
            super::trace::live_dalepena_after_dctl(read(DALEPENA));
        }
        saved_config
    }
}

/// Apply Linux's USB2 PHY guard around a DWC3 Run/Stop transition.
///
/// `dwc3_gadget_run_stop()` clears SUSPHY and ENBLSLPM before writing DCTL,
/// waits for DEVCTRLHLT, and restores the saved bits afterwards. Keeping that
/// sequence in one helper prevents the Fastboot handoff and runtime-PM paths
/// from diverging at exactly the transition where DWC3 is most sensitive to
/// a stale USB2 low-power state.
pub(super) unsafe fn run_stop_device(is_on: bool) -> bool {
    unsafe {
        let saved_config = prepare_run_stop_device(is_on);
        let complete = wait_device_state(!is_on);
        #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reassert_runstop)]
        if is_on {
            let snpsid = read(GSNPSID);
            let ip = snpsid >> 16;
            if (ip == DWC31_IP || ip == DWC32_IP) && read(DCTL) & DCTL_RUN_STOP == 0 {
                // Some DWC_usb31 handoffs accept the first Run/Stop write but
                // clear it while the device-state transition synchronizes.
                // Reapply only the start bit after the wait so the SS PHY and
                // endpoint programming remain untouched.
                let dctl = run_stop_value(read(DCTL), snpsid);
                write(DCTL, dctl);
                let readback = read(DCTL);
                trace_event(TRACE_DWC3_HALTED, 0x5253_5354, readback, read(DSTS), 0, 0);
            }
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_hold_runstop)]
        if is_on {
            let snpsid = read(GSNPSID);
            let ip = snpsid >> 16;
            if ip == DWC31_IP || ip == DWC32_IP {
                // A DWC_usb31 handoff can clear RUN_STOP while its SS link
                // state machine is still leaving SS_DIS.  Keep this A/B
                // deliberately limited to the start bit: no PHY, endpoint,
                // event-ring, or transfer-resource state is rewritten.
                // Reassert for the same bounded window used by the SS EP0
                // arm retry, so the host can observe whether the controller
                // accepts the request once link training has progressed.
                let deadline = super::arch_counter()
                    .saturating_add(super::arch_counter_frequency().saturating_mul(5_000) / 1000);
                let mut writes = 0u32;
                let mut last_dctl = read(DCTL);
                while super::arch_counter() < deadline {
                    last_dctl = read(DCTL);
                    if last_dctl & DCTL_RUN_STOP != 0 {
                        break;
                    }
                    let dctl = run_stop_value(last_dctl, snpsid);
                    write(DCTL, dctl);
                    last_dctl = read(DCTL);
                    writes = writes.saturating_add(1);
                    crate::timer::delay_us(200);
                }
                trace_event(
                    TRACE_DWC3_HALTED,
                    0x4852_5354, // "HRST": held RUN_STOP result
                    writes,
                    last_dctl,
                    read(DSTS),
                    u32::from(last_dctl & DCTL_RUN_STOP != 0),
                );
            }
        }
        if saved_config != 0 {
            let current = read(GUSB2PHYCFG0);
            write(GUSB2PHYCFG0, current | saved_config);
        }
        complete
    }
}

/// The Run/Stop write without the DEVCTRLHLT readback wait. Gate runs must
/// return to the probe before the attach-triggered ~17 s biter window
/// closes, and the stale-halt readback timeout (up to 2 s, the common
/// "RUN/STOP readback timed out; continuing" case) is the handoff's slowest
/// bounded wait. The physical pull-up transition is identical to
/// `run_stop_device(true)`; only the status wait is skipped.
pub(super) unsafe fn run_stop_device_no_readback(is_on: bool) -> bool {
    unsafe {
        let saved_config = prepare_run_stop_device(is_on);
        if saved_config != 0 {
            let current = read(GUSB2PHYCFG0);
            write(GUSB2PHYCFG0, current | saved_config);
        }
        true
    }
}
