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
        // Match Linux's reconnect path: clear stale endpoint/device state
        // without touching the already-running Qualcomm PHY and clock
        // branches. RUN_STOP must be cleared in the same write; preserving
        // Fastboot's RUN_STOP bit can leave the device half-running while
        // CSFTRST is asserted.
        let mut dctl = initial_dctl;
        dctl |= DCTL_CSFTRST;
        dctl &= !DCTL_RUN_STOP;
        write_dctl_safe(dctl);
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
        let retries = if slow_reset { 10 } else { 1_000 };
        let mut device_reset_complete = false;
        for _ in 0..retries {
            if read(DCTL) & DCTL_CSFTRST == 0 {
                device_reset_complete = true;
                break;
            }
            if slow_reset {
                crate::timer::delay_ms(20);
            } else {
                crate::timer::delay_us(1);
            }
        }
        if !device_reset_complete {
            log_puts("usb: DWC3 device reset timeout\n");
            return false;
        }

        // Upstream Linux waits an additional 50 ms only for DWC_usb31 1.80a
        // and earlier before accessing its PHY domain. DWC3/1.90a+ do not
        // require that legacy synchronization delay.
        if ip == DWC31_IP && version <= DWC31_REVISION_180A {
            crate::timer::delay_ms(50);
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
/// buffer setup preserves the complete GEVNTCOUNT register (including EHB),
/// while the Run/Stop halt contract consumes only the byte count and advances
/// the software cursor just as Linux advances `ev_buf->lpos`.
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

/// Apply Linux's USB2 PHY guard around a DWC3 Run/Stop transition.
///
/// `dwc3_gadget_run_stop()` clears SUSPHY and ENBLSLPM before writing DCTL,
/// waits for DEVCTRLHLT, and restores the saved bits afterwards. Keeping that
/// sequence in one helper prevents the Fastboot handoff and runtime-PM paths
/// from diverging at exactly the transition where DWC3 is most sensitive to
/// a stale USB2 low-power state.
pub(super) unsafe fn run_stop_device(is_on: bool) -> bool {
    unsafe {
        let mut usb2 = read(GUSB2PHYCFG0);
        let saved_config = usb2 & (GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        if saved_config != 0 {
            usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
            write(GUSB2PHYCFG0, usb2);
        }

        let mut dctl = read(DCTL);
        if is_on {
            dctl = run_stop_value(dctl, read(GSNPSID));
        } else {
            dctl &= !DCTL_RUN_STOP;
        }
        write_dctl_safe(dctl);
        let complete = wait_device_state(!is_on);

        if saved_config != 0 {
            let current = read(GUSB2PHYCFG0);
            write(GUSB2PHYCFG0, current | saved_config);
        }
        complete
    }
}
