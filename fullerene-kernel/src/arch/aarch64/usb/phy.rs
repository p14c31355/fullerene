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
            // One marker per table entry makes the last committed index the
            // upper bound when a particular QMP register access aborts.
            trace_marker(
                TRACE_PROBE_WATCHDOG,
                0x514d_0000 | (index as u32 & 0xff),
            );
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
        for _ in 0..1_000_000 {
            if read_volatile(qmp_reg(pcs_status)) & QMP_PHYSTATUS == 0 {
                trace_marker(TRACE_PROBE_WATCHDOG, 0x514d_4f4b); // "QMOK"
                if qmp_phase_probe_stop(8) {
                    return true;
                }
                return true;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
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
            let mut value = read_volatile(qmp_reg(autonomous));
            value &= !(QMP_ARCVR_DTCT_EN | QMP_ALFPS_DTCT_EN | QMP_ARCVR_DTCT_EVENT_SEL);
            write_volatile(qmp_reg(autonomous), value);
            qmp_clear_lfps_rxterm_irq();
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
        let hsphy_param_override =
            core::ptr::read_volatile(core::ptr::addr_of!(ACTIVE_HSPHY_PARAM_OVERRIDE));
        for &(offset, value) in hsphy_param_override.iter() {
            // The production Bramble/Barbet table has only two QUSB2
            // overrides. A trailing sentinel preserves the fixed table shape
            // and must be skipped exactly like the DT's absent third entry.
            if offset == usize::MAX {
                continue;
            }
            write_volatile(hsphy_reg(offset), value);
        }

        // Android's msm_hsphy_init() enables the external termination tune
        // unless the DT explicitly says that no external resistor is present
        // or supplies an efuse RCAL code.  Bramble's usb2_phy0 node has
        // neither qcom,no-rext-present nor a qcom,rcal-reg/rcal-mask pair,
        // so the source-equivalent path is RTUNE_SEL=1.
        hsphy_update(HSPHY_RTUNE_SEL, 1, 1);

        // phy-msm-snps-hs.c stops here for the analog setup: VREGBYPASS,
        // the suspend-N hold, SLEEPM, then the POR release. The earlier
        // RTUNE_SEL write is the source-equivalent femto-PHY termination
        // setup. Factory ABL's usb_shared_hs_phy_init() additionally clears
        // the old QUSB ATE/test state before releasing the PHY; keep that
        // extra sequence opt-in so it remains an isolated A/B variable.
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
        // Wait for the PLL to lock after POR release. The SNPS femto-PHY's
        // PLL needs a bounded settling time before it can recover the HS
        // clock from incoming USB traffic. Without this delay, the PHY can
        // drive D+ (pull-up) and answer the host's chirp handshake (which
        // is a simple D- drive), but cannot receive HS data (SOF, SETUP),
        // which requires a locked PLL for clock recovery. The QUSB2 driver
        // uses usleep_range(150, 160) after POWER_DOWN clear; match that
        // lower bound for the femto-PHY's POR release.
        crate::timer::delay_us(150);
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

/// Apply the DWC3 post-reset reference-clock calibration from
/// dwc3_msm_update_ref_clk(). The GCC source clock is managed separately by
/// the Bramble platform layer; this only programs the controller's timing
/// registers after a core reset.
pub(super) unsafe fn update_dwc3_ref_clock() {
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
