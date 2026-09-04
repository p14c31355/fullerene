//! DWC3 PHY and Qualcomm clock-source bring-up.

use core::ptr::{read_volatile, write_volatile};

use super::config::qscratch_set;
use super::log::log_puts;
use super::mmio::*;
use super::phy_tables::{ACTIVE_HSPHY_PARAM_OVERRIDE, ACTIVE_QMP_INIT, ACTIVE_QMP_INIT_DELAY_US};
use super::trace::{TRACE_PROBE_WATCHDOG, TRACE_UTMI_CLOCK, trace_event, trace_marker};

/// Current-boot result for the optional QMP phase probe. This is deliberately
/// outside the retained DRAM trace: the caller consumes it immediately after
/// `init_qmp_phy()` returns, before any later boot can scribble the trace.
static mut QMP_PHASE_PROBE_REACHED: u32 = 0;

#[inline(always)]
unsafe fn qmp_mb() {
    // Linux's arm64 `mb()` used by the Qualcomm driver is a system DMB.  The
    // QMP writes are relaxed MMIO accesses; this orders them before the next
    // PHY state transition without turning every table write into a DSB.
    core::arch::asm!("dmb sy", options(nostack, preserves_flags));
}

#[inline(always)]
unsafe fn qmp_wmb() {
    // Linux arm64 defines wmb() as dsb(st). The QMP resume path uses this
    // stronger write-only barrier after its relaxed PHY writes.
    core::arch::asm!("dsb st", options(nostack, preserves_flags));
}

#[inline(always)]
fn qmp_phase_probe_selected(phase: u32) -> bool {
    match (option_env!("FULLERENE_USB_QMP_PHASE_STOP"), phase) {
        (Some("1"), 1)
        | (Some("2"), 2)
        | (Some("3"), 3)
        | (Some("4"), 4)
        | (Some("5"), 5)
        | (Some("6"), 6)
        | (Some("7"), 7)
        | (Some("8"), 8) => true,
        _ => false,
    }
}

#[inline(always)]
unsafe fn qmp_phase_probe_stop(phase: u32) -> bool {
    if !qmp_phase_probe_selected(phase) {
        return false;
    }
    QMP_PHASE_PROBE_REACHED = phase;
    true
}

pub fn qmp_phase_probe_reached() -> u32 {
    unsafe { QMP_PHASE_PROBE_REACHED }
}

pub fn qmp_phase_probe_requested() -> bool {
    option_env!("FULLERENE_USB_QMP_PHASE_STOP").is_some()
}

/// Pack the QMP state needed by the post-Run/Stop readout without changing
/// any PHY state. Bits 15:0 are PCS_STATUS1, bits 23:16 are
/// PCS_START_CONTROL, and bits 31:24 are COM_TYPEC_CTRL.
pub(super) unsafe fn qmp_post_runstop_snapshot() -> u32 {
    let pcs_status = qmp_contract_offset(0, QMP_PCS_STATUS1);
    let pcs_start = qmp_contract_offset(5, QMP_PCS_START_CONTROL);
    let typec = qmp_contract_offset(12, QMP_COM_TYPEC_CTRL);
    let status = read_volatile(qmp_reg(pcs_status));
    let start = read_volatile(qmp_reg(pcs_start));
    let lane = read_volatile(qmp_reg(typec));
    (status & 0xffff) | ((start & 0xff) << 16) | ((lane & 0xff) << 24)
}

/// Pack the QMP common/PCS power-control readbacks separately from the
/// existing status/start/lane snapshot. The Android driver writes `1` to both
/// controls to power the PHY up; retaining these bits distinguishes a live
/// register window from a PHY that has silently returned to power-down.
pub(super) unsafe fn qmp_power_snapshot() -> u32 {
    let com_power_down = qmp_contract_offset(8, QMP_COM_POWER_DOWN_CTRL);
    let pcs_power_down = qmp_contract_offset(3, QMP_PCS_POWER_DOWN_CONTROL);
    let com = read_volatile(qmp_reg(com_power_down));
    let pcs = read_volatile(qmp_reg(pcs_power_down));
    (com & 1) | ((pcs & 1) << 1)
}

/// Read the QMP PCS_STATUS2 link-training indicator without changing PHY
/// state. On the Bramble/Lito combo PHY, bit 3 is
/// `RX_EQUALIZATION_IN_PROGRESS`, the same status that Android's optional
/// link-training-reset workaround polls before toggling INSIG controls.
pub(super) unsafe fn qmp_status2_snapshot() -> u32 {
    let status2 = qmp_contract_offset(15, QMP_PCS_STATUS2);
    read_volatile(qmp_reg(status2))
}

/// Re-assert the Android QMP power-up writes without resetting the PHY or
/// replaying its initialization table. This is an isolated ownership/power
/// A/B for a no-core handoff where the controls read back as zero.
pub(super) unsafe fn qmp_reassert_power() -> u32 {
    let com_power_down = qmp_contract_offset(8, QMP_COM_POWER_DOWN_CTRL);
    let pcs_power_down = qmp_contract_offset(3, QMP_PCS_POWER_DOWN_CONTROL);
    write_volatile(qmp_reg(com_power_down), 0x01);
    write_volatile(qmp_reg(pcs_power_down), 0x01);
    qmp_mb();
    qmp_power_snapshot()
}

/// Match the Bramble QMP PHY's `usb_phy_notify_disconnect()` callback.
/// `dwc3_otg_start_peripheral(..., 0)` invokes this before suspending the
/// connected SuperSpeed PHY; the callback writes zero to the PCS power-down
/// control and performs a readback before the later PHY reset/init boundary.
pub(super) unsafe fn qmp_notify_disconnect() {
    let power_down = qmp_contract_offset(3, QMP_PCS_POWER_DOWN_CONTROL);
    write_volatile(qmp_reg(power_down), 0);
    let readback = read_volatile(qmp_reg(power_down));
    super::log::log_hex("usb: QMP disconnect power-down=", u64::from(readback));
}

pub(super) unsafe fn init_qmp_phy() -> bool {
    // Keep the failure boundary in retained trace as well as UART. A Bramble
    // watchdog can reset the phone before the Fullerene log is readable, and
    // the QMP block is precisely the kind of MMIO window where the access
    // immediately following a marker may be the one that aborts.
    trace_marker(TRACE_PROBE_WATCHDOG, 0x514d_5042); // "QMPB"
    if qmp_phase_probe_stop(1) {
        return true;
    }
    let com_power_down = qmp_contract_offset(8, QMP_COM_POWER_DOWN_CTRL);
    let pcs_power_down = qmp_contract_offset(3, QMP_PCS_POWER_DOWN_CONTROL);
    let reset_override = qmp_contract_offset(10, QMP_COM_RESET_OVRD_CTRL);
    let typec = qmp_contract_offset(12, QMP_COM_TYPEC_CTRL);
    let phy_mode = qmp_contract_offset(11, QMP_COM_PHY_MODE_CTRL);
    let com_sw_reset = qmp_contract_offset(9, QMP_COM_SW_RESET);
    let pcs_sw_reset = qmp_contract_offset(4, QMP_PCS_SW_RESET);
    let pcs_start = qmp_contract_offset(5, QMP_PCS_START_CONTROL);
    let pcs_status = qmp_contract_offset(0, QMP_PCS_STATUS1);
    unsafe {
        trace_marker(TRACE_PROBE_WATCHDOG, 0x514d_4350); // "QMCP"
        if qmp_phase_probe_stop(2) {
            return true;
        }
        // Match msm_ssphy_qmp_init(): power the common and PCS blocks before
        // selecting the Type-C lane and USB+DP combo mode. The lane value is
        // 2 for lane A and 3 for lane B, as used by the Android QMP driver.
        write_volatile(qmp_reg(com_power_down), 0x01);
        write_volatile(qmp_reg(pcs_power_down), 0x01);
        qmp_mb();
        // Keep the PMIC observer and the QMP lane decision independently
        // testable. The explicit build-time A/B override changes only the
        // QMP TYPEC_CTRL write; it never writes Type-C role/VBUS registers.
        let lane = match option_env!("FULLERENE_USB_QMP_LANE") {
            Some("a") => 0x02,
            Some("b") => 0x03,
            _ if super::TYPEC_LANE_B => 0x03,
            _ => 0x02,
        };
        write_volatile(qmp_reg(reset_override), 0x0f);
        write_volatile(qmp_reg(typec), lane);
        let _ = read_volatile(qmp_reg(typec));
        write_volatile(qmp_reg(phy_mode), 0x03);
        let _ = read_volatile(qmp_reg(phy_mode));
        write_volatile(qmp_reg(reset_override), 0x00);
        qmp_mb();

        // msm_ssphy_qmp_init() calls usb_qmp_powerup_phy() once from
        // usb_qmp_update_portselect_phymode() and once again immediately
        // before the main table. Preserve that second power-up boundary;
        // combo-PHY reset/mode selection is not interchangeable with the
        // post-selection common/PCS power-up write.
        write_volatile(qmp_reg(com_power_down), 0x01);
        write_volatile(qmp_reg(pcs_power_down), 0x01);
        qmp_mb();

        trace_marker(TRACE_PROBE_WATCHDOG, 0x514d_5442); // "QMTB"
        if qmp_phase_probe_stop(3) {
            return true;
        }
        let qmp_init = core::ptr::read_volatile(core::ptr::addr_of!(ACTIVE_QMP_INIT));
        let qmp_init_delays =
            core::ptr::read_volatile(core::ptr::addr_of!(ACTIVE_QMP_INIT_DELAY_US));
        for (index, (&(offset, value), &delay_us)) in
            qmp_init.iter().zip(qmp_init_delays.iter()).enumerate()
        {
            // Sample the table index so QMP setup cannot consume the retained
            // trace ring before the phase markers and final QMOK record.
            if index % 16 == 0 || index + 1 == qmp_init.len() {
                trace_marker(TRACE_PROBE_WATCHDOG, 0x514d_0000 | (index as u32 & 0xff));
            }
            write_volatile(qmp_reg(offset), value);
            if delay_us != 0 {
                crate::timer::delay_us(delay_us as u64);
            }
        }

        trace_marker(TRACE_PROBE_WATCHDOG, 0x514d_5445); // "QMTE"
        if qmp_phase_probe_stop(4) {
            return true;
        }
        write_volatile(qmp_reg(com_sw_reset), 0x00);
        write_volatile(qmp_reg(pcs_sw_reset), 0x00);
        trace_marker(TRACE_PROBE_WATCHDOG, 0x514d_5354); // "QMST"
        if qmp_phase_probe_stop(5) {
            return true;
        }
        write_volatile(qmp_reg(pcs_start), 0x03);
        qmp_mb();
        trace_marker(TRACE_PROBE_WATCHDOG, 0x514d_5352); // "QMSR"
        if qmp_phase_probe_stop(6) {
            return true;
        }
        let _ = read_volatile(qmp_reg(pcs_status));
        trace_marker(TRACE_PROBE_WATCHDOG, 0x514d_504c); // "QMPL"
        if qmp_phase_probe_stop(7) {
            return true;
        }
        for attempt in 0..1_000 {
            if read_volatile(qmp_reg(pcs_status)) & QMP_PHYSTATUS == 0 {
                trace_marker(TRACE_PROBE_WATCHDOG, 0x514d_4f4b); // "QMOK"
                if qmp_phase_probe_stop(8) {
                    return true;
                }
                return true;
            }
            // Android's msm_ssphy_qmp_init() waits usleep_range(1, 2) between
            // PHYSTATUS samples, so the loop has real-time meaning: at most
            // ~1 ms of settling, sampled every microsecond. A back-to-back
            // MMIO loop is a different time base entirely, and the UTMI
            // clock-source helper above already switched to
            // crate::timer::delay_us() for the same reason. Keep the
            // historical nop-loop behavior behind an opt-out A/B.
            if cfg!(fullerene_aarch64_usb_qmp_poll_nop_loop) {
                core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
            } else {
                crate::timer::delay_us(1);
            }
            let _ = attempt;
        }
    }
    log_puts("usb: QMP PHY initialization timeout\n");
    false
}

/// Clear the QMP LFPS receiver-detect interrupt using the required 1 -> 0
/// sequence from msm-ssusb-qmp. A readback between the writes is not needed
/// by the PHY, but the compiler/MMIO ordering barrier is: the second write
/// must not be observed before the clear is asserted.
pub(super) unsafe fn qmp_clear_lfps_rxterm_irq() {
    let clear = qmp_contract_offset(2, QMP_PCS_LFPS_RXTERM_IRQ_CLEAR);
    unsafe {
        write_volatile(qmp_reg(clear), QMP_LFPS_IRQ_CLEAR);
        #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_qmp_lfps_clear_wmb)]
        qmp_wmb();
        #[cfg(not(fullerene_aarch64_usb_gadget_handoff_ss_qmp_lfps_clear_wmb))]
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        write_volatile(qmp_reg(clear), 0);
        let _ = read_volatile(qmp_reg(clear));
    }
}

/// Match msm_ssusb_qmp_enable_autonomous()/disable_autonomous_mode() for the
/// Lito USB+DP combo PHY. The device-side SuperSpeed mode enables both
/// receiver-detect and LFPS detection; the receiver-detect event-select bit
/// stays clear in that mode. Autonomous mode also turns on the PCS I/O clamp
/// (the register is active-high for disabling the clamp, hence clear it when
/// enabling autonomous operation).
pub(super) unsafe fn qmp_set_autonomous_mode(enable: bool) {
    let autonomous = qmp_contract_offset(1, QMP_PCS_AUTONOMOUS_MODE_CTRL);
    let clamp_offset = qmp_contract_offset(14, QMP_PCS_CLAMP_ENABLE);
    unsafe {
        if enable {
            qmp_clear_lfps_rxterm_irq();
            let mut value = read_volatile(qmp_reg(autonomous));
            value &= !(QMP_ARCVR_DTCT_EN | QMP_ALFPS_DTCT_EN | QMP_ARCVR_DTCT_EVENT_SEL);
            value |= QMP_ARCVR_DTCT_EN | QMP_ALFPS_DTCT_EN;
            write_volatile(qmp_reg(autonomous), value);
            // Android's combo-PHY path calls clamp_enable(true), which
            // writes !true to this active-high clamp control.
            let mut clamp = read_volatile(qmp_reg(clamp_offset));
            clamp &= !QMP_CLAMP_EN;
            write_volatile(qmp_reg(clamp_offset), clamp);
            let _ = read_volatile(qmp_reg(autonomous));
        } else {
            // Resume first releases the clamp, then disables both autonomous
            // detectors, and finally clears any receiver-detect edge left by
            // the suspended PHY.
            let mut clamp = read_volatile(qmp_reg(clamp_offset));
            clamp |= QMP_CLAMP_EN;
            write_volatile(qmp_reg(clamp_offset), clamp);
            #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_clear_qmp_autonomous_exact)]
            write_volatile(qmp_reg(autonomous), 0);
            #[cfg(not(fullerene_aarch64_usb_gadget_handoff_ss_clear_qmp_autonomous_exact))]
            {
                let mut value = read_volatile(qmp_reg(autonomous));
                value &= !(QMP_ARCVR_DTCT_EN | QMP_ALFPS_DTCT_EN | QMP_ARCVR_DTCT_EVENT_SEL);
                write_volatile(qmp_reg(autonomous), value);
            }
            qmp_clear_lfps_rxterm_irq();
            #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_qmp_resume_wmb)]
            qmp_wmb();
        }
    }
}

/// Apply the small, non-calibration portion of the SM7250 USB2 PHY setup.
///
/// The full Linux driver also obtains regulators and a 19.2 MHz reference
/// clock from the board description. Those are already left on by the
/// Pixel boot chain; the register sequence below is the actual PHY setup
/// from the `qcom,usb-hsphy-snps-femto` driver and the Bramble override
/// sequence in its device tree.
pub(super) unsafe fn init_hsphy() {
    unsafe { init_hsphy_inner(false) }
}

/// Run the Bramble qpr1 `msm_hsphy_init()` register sequence without the
/// legacy handoff helper's extra RTUNE write or post-POR delay.  Power,
/// reference-clock, and reset ownership are established by the caller at the
/// same boundary as the official DWC3 soft-reset path.
pub(super) unsafe fn init_hsphy_source_exact() {
    unsafe { init_hsphy_inner(true) }
}

unsafe fn init_hsphy_inner(source_exact: bool) {
    unsafe {
        hsphy_update(
            HSPHY_CFG0,
            HSPHY_CFG0_CMN_CTRL_OVERRIDE_EN,
            HSPHY_CFG0_CMN_CTRL_OVERRIDE_EN,
        );
        hsphy_update(HSPHY_UTMI_CTRL5, HSPHY_UTMI_POR, HSPHY_UTMI_POR);
        hsphy_update(HSPHY_COMMON0, HSPHY_COMMON0_FSEL_MASK, 0);
        hsphy_update(
            HSPHY_COMMON1,
            HSPHY_COMMON1_PLLBTUNE,
            HSPHY_COMMON1_PLLBTUNE,
        );
        hsphy_update(HSPHY_REFCLK_CTRL, 0x3, 0x2);
        hsphy_update(
            HSPHY_COMMON1,
            HSPHY_COMMON1_VBUSVLDEXTSEL0,
            HSPHY_COMMON1_VBUSVLDEXTSEL0,
        );
        hsphy_update(
            HSPHY_CTRL1,
            HSPHY_CTRL1_VBUSVLDEXT0,
            HSPHY_CTRL1_VBUSVLDEXT0,
        );

        // qcom,param-override-seq is encoded as (value, register offset).
        let mut hsphy_param_override =
            core::ptr::read_volatile(core::ptr::addr_of!(ACTIVE_HSPHY_PARAM_OVERRIDE));
        #[cfg(fullerene_aarch64_usb_hsphy_qrd_override)]
        {
            // The Pixel QRD overlay uses 0xc8 for QUSB2 TUNE2 at 0x70,
            // while the factory Bramble DT uses 0x85. Keep this as an
            // opt-in physical A/B; the normal DT-selected value is intact.
            hsphy_param_override[1] = (0x70, 0xc8);
        }
        for &(offset, value) in hsphy_param_override.iter() {
            // The production Bramble/Barbet table has only two QUSB2
            // overrides. A trailing sentinel preserves the fixed table shape
            // and must be skipped exactly like the DT's absent third entry.
            if offset == usize::MAX {
                continue;
            }
            write_volatile(hsphy_reg(offset), value);
            hsphy_write_barrier();
        }

        // The Bramble qpr1 `msm_hsphy_init()` body does not write RTUNE_SEL;
        // retain the old local write only for the pre-existing non-exact
        // helper paths.
        if !source_exact || cfg!(fullerene_aarch64_usb_hsphy_rtune) {
            hsphy_update(HSPHY_RTUNE_SEL, 1, 1);
        }

        // phy-msm-snps-hs.c continues with VREGBYPASS, the suspend-N hold,
        // SLEEPM, POR release, suspend-N select clear, and common-control
        // override release. Factory ABL's usb_shared_hs_phy_init()
        // additionally clears the old QUSB ATE/test state before releasing
        // the PHY; keep that extra sequence opt-in so it remains an isolated
        // A/B variable.
        hsphy_update(
            HSPHY_COMMON2,
            HSPHY_COMMON2_VREGBYPASS,
            HSPHY_COMMON2_VREGBYPASS,
        );
        if cfg!(fullerene_aarch64_usb_abl_shared_hsphy) {
            hsphy_update(HSPHY_UTMI_CTRL5, HSPHY_UTMI_ATE_RESET, 0);
            hsphy_update(
                HSPHY_TEST1,
                HSPHY_TEST1_TESTDATAOUTSEL | HSPHY_TEST1_TOGGLE_2WR,
                0,
            );
            hsphy_update(HSPHY_COMMON0, HSPHY_COMMON0_VATESTENB_MASK, 0);
            hsphy_update(HSPHY_TEST0, HSPHY_TEST0_DATA_MASK, 0);
        }
        hsphy_update(
            HSPHY_CTRL2,
            HSPHY_CTRL2_SUSPEND_N_SEL | HSPHY_CTRL2_SUSPEND_N,
            HSPHY_CTRL2_SUSPEND_N_SEL | HSPHY_CTRL2_SUSPEND_N,
        );
        hsphy_update(HSPHY_UTMI_CTRL0, HSPHY_UTMI_SLEEPM, HSPHY_UTMI_SLEEPM);
        hsphy_update(HSPHY_UTMI_CTRL5, HSPHY_UTMI_POR, 0);
        // The official SNPS femto-PHY init has no delay at this boundary. The
        // old local helper's 150 us wait is retained only outside the exact
        // source-confirmed A/B. The reviewer A/B restores it even in the
        // source-exact path: Android's equivalent settling time lives in the
        // external reset hold (msm_hsphy_reset's 100-150 us), so a handoff
        // that pulses the BCR line may still owe the analog block settle time
        // here that the official driver never needs at this exact spot.
        #[cfg(fullerene_aarch64_usb_hsphy_por_delay_150)]
        crate::timer::delay_us(150);
        #[cfg(not(fullerene_aarch64_usb_hsphy_por_delay_150))]
        if !source_exact {
            crate::timer::delay_us(150);
        }
        hsphy_update(HSPHY_CTRL2, HSPHY_CTRL2_SUSPEND_N_SEL, 0);
        if cfg!(fullerene_aarch64_usb_abl_shared_hsphy) {
            // ABL waits after dropping SUSPEND_N_SEL before releasing the
            // common-control override.
            crate::timer::delay_us(20);
        }
        hsphy_update(HSPHY_CFG0, HSPHY_CFG0_CMN_CTRL_OVERRIDE_EN, 0);
        if cfg!(fullerene_aarch64_usb_abl_shared_hsphy) {
            // It then gives the analog block another 20 us to settle.
            crate::timer::delay_us(20);
        }
    }
}

pub(super) unsafe fn select_utmi_pipe_clock() {
    // This is the Qualcomm glue sequence used when DWC3 operates without a
    // SuperSpeed PHY. It prevents the absent QMP PIPE clock from holding the
    // core in reset while the USB2 UTMI clock is already running.
    trace_event(TRACE_UTMI_CLOCK, 0, 0, 0, 0, 0);
    unsafe {
        qscratch_set(QSCRATCH_GENERAL_CFG, PIPE_UTMI_CLK_DIS);
        // dwc3_qcom_select_utmi_clk() uses usleep_range(100, 1000) between
        // each clock-source transition.  A fixed architectural delay keeps
        // the lower bound independent of the boot CPU frequency; a NOP loop
        // could be shorter than 100 us on a fast handset.
        crate::timer::delay_us(100);
        qscratch_set(QSCRATCH_GENERAL_CFG, PIPE_UTMI_CLK_SEL | PIPE3_PHYSTATUS_SW);
        crate::timer::delay_us(100);
        let value = read_qscratch(QSCRATCH_GENERAL_CFG) & !PIPE_UTMI_CLK_DIS;
        write_qscratch(QSCRATCH_GENERAL_CFG, value);
    }
    trace_event(TRACE_UTMI_CLOCK, 1, 0, 0, 0, 0);
}

/// Reproduce qpr1's `DWC3_CONTROLLER_POST_RESET_EVENT` USB2-only mux turn.
///
/// The Qualcomm glue uses a much shorter 2--5 us interval in this callback
/// than the standalone UTMI clock-source helper above. The distinction is
/// important for the direct Fastboot handoff: qpr1 runs this immediately
/// after `dwc3_core_init()` returns, before gadget endpoint state is built.
/// Keep the three read-modify-write steps and the final clear separate so a
/// retained trace can identify this post-reset boundary.
pub(super) unsafe fn select_utmi_pipe_clock_post_reset() {
    trace_event(TRACE_UTMI_CLOCK, 2, 0, 0, 0, 0);
    unsafe {
        qscratch_set(QSCRATCH_GENERAL_CFG, PIPE_UTMI_CLK_DIS);
        crate::timer::delay_us(3);
        qscratch_set(QSCRATCH_GENERAL_CFG, PIPE_UTMI_CLK_SEL | PIPE3_PHYSTATUS_SW);
        crate::timer::delay_us(3);
        let value = read_qscratch(QSCRATCH_GENERAL_CFG) & !PIPE_UTMI_CLK_DIS;
        write_qscratch(QSCRATCH_GENERAL_CFG, value);
    }
    trace_event(TRACE_UTMI_CLOCK, 3, 0, 0, 0, 0);
}

/// Apply the historical controller reference-clock calibration retained by
/// Fullerene's earlier handoff experiments.
///
/// The Bramble qpr1 `dwc3-msm.c` source has no `dwc3_msm_update_ref_clk()`
/// helper and does not write these `GUCTL`/extended `GFLADJ` fields. The
/// source-confirmed A/B therefore has an opt-in path that skips this function
/// and preserves the firmware's register state.
pub(super) unsafe fn update_dwc3_ref_clock() {
    #[cfg(not(fullerene_aarch64_usb_gadget_handoff_ss_preserve_ref_clock_state))]
    unsafe {
        let guctl = read(GUCTL);
        write(
            GUCTL,
            (guctl & !GUCTL_REFCLKPER_MASK) | GUCTL_REFCLKPER_19_2MHZ,
        );
        if read(GSNPSID) >= DWC3_REVISION_250A {
            let gfladj = read(GFLADJ);
            write(
                GFLADJ,
                (gfladj
                    & !(GFLADJ_REFCLK_FLADJ_MASK
                        | GFLADJ_REFCLK_LPM_SEL
                        | GFLADJ_REFCLK_240MHZ_DECR
                        | GFLADJ_REFCLK_240MHZDECR_PLS1))
                    | GFLADJ_REFCLK_LPM_SEL
                    | GFLADJ_REFCLK_240MHZ_DECR
                    | GFLADJ_REFCLK_240MHZDECR_PLS1
                    | GFLADJ_REFCLK_FLADJ_19_2MHZ,
            );
        }
    }
}
