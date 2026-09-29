//! DSI PHY (`qcom,dsi-phy-v4.1`, 7nm) - port of the upstream driver's math and
//! sequences for Lito. Sources are quoted per item; see
//! `docs/DISPLAY_REGISTERS.md` for the register map and the full extraction.
//!
//! Stage C of `docs/DISPLAY_BRINGUP.md`. This file starts with the parts that are
//! pure arithmetic, because those are verifiable on the host without hardware.

/// PHY PLL reference clock (`dsi_phy_7nm.c:44`).
pub const VCO_REF_CLK_RATE: u64 = 19_200_000;

/// DSI PHY 0 register windows (vendor DT `lito-sde.dtsi:642-647`).
pub const PHY_CMN_BASE: usize = 0x0ae9_4400;
pub const PHY_LANE_BASE: usize = 0x0ae9_4400;
pub const PHY_PLL_BASE: usize = 0x0ae9_4400;

/// `DSI_7nm_PHY_CMN` offsets (`dsi_phy_7nm.xml`, domain `DSI_7nm_PHY_CMN`).
pub mod cmn {
    pub const REVISION_ID0: usize = 0x000;
    pub const CLK_CFG0: usize = 0x010;
    pub const CLK_CFG1: usize = 0x014;
    pub const GLBL_CTRL: usize = 0x018;
    pub const RBUF_CTRL: usize = 0x01c;
    pub const VREG_CTRL_0: usize = 0x020;
    pub const CTRL_0: usize = 0x024;
    pub const CTRL_1: usize = 0x028;
    pub const CTRL_2: usize = 0x02c;
    pub const CTRL_3: usize = 0x030;
    pub const LANE_CFG0: usize = 0x034;
    pub const LANE_CFG1: usize = 0x038;
    pub const PLL_CNTRL: usize = 0x03c;
    pub const LANE_CTRL0: usize = 0x0a0;
    pub const TIMING_CTRL_0: usize = 0x0b4;
    pub const TIMING_CTRL_1: usize = 0x0b8;
    pub const TIMING_CTRL_2: usize = 0x0bc;
    pub const TIMING_CTRL_3: usize = 0x0c0;
    pub const TIMING_CTRL_4: usize = 0x0c4;
    pub const TIMING_CTRL_5: usize = 0x0c8;
    pub const TIMING_CTRL_6: usize = 0x0cc;
    pub const TIMING_CTRL_7: usize = 0x0d0;
    pub const TIMING_CTRL_8: usize = 0x0d4;
    pub const TIMING_CTRL_9: usize = 0x0d8;
    pub const TIMING_CTRL_10: usize = 0x0dc;
    pub const TIMING_CTRL_11: usize = 0x0e0;
    pub const TIMING_CTRL_12: usize = 0x0e4;
    pub const TIMING_CTRL_13: usize = 0x0e8;
    pub const GLBL_HSTX_STR_CTRL_0: usize = 0x0ec;
    pub const GLBL_RESCODE_OFFSET_TOP_CTRL: usize = 0x0f4;
    pub const GLBL_RESCODE_OFFSET_BOT_CTRL: usize = 0x0f8;
    pub const GLBL_LPTX_STR_CTRL: usize = 0x100;
    pub const GLBL_PEMPH_CTRL_0: usize = 0x104;
    pub const GLBL_STR_SWI_CAL_SEL_CTRL: usize = 0x10c;
    pub const VREG_CTRL_1: usize = 0x110;
    pub const CTRL_4: usize = 0x114;
    pub const GLBL_DIGTOP_SPARE4: usize = 0x128;
    pub const PHY_STATUS: usize = 0x140;
    pub const LANE_STATUS0: usize = 0x148;
    pub const LANE_STATUS1: usize = 0x14c;
}

/// `DSI_7nm_PHY` per-lane block; 5 instances at `0x100` stride.
pub mod lane {
    pub const STRIDE: usize = 0x100;
    pub const CFG0: usize = 0x00;
    pub const CFG1: usize = 0x04;
    pub const CFG2: usize = 0x08;
    pub const PIN_SWAP: usize = 0x10;
    pub const LPRX_CTRL: usize = 0x14;
    pub const TX_DCTRL: usize = 0x0c;
}

/// `DSI_7nm_PHY_PLL` offsets (`dsi_phy_7nm.xml`, domain `DSI_7nm_PHY_PLL`).
pub mod pll {
    pub const ANALOG_CONTROLS_TWO: usize = 0x004;
    pub const ANALOG_CONTROLS_THREE: usize = 0x010;
    pub const ANALOG_CONTROLS_FIVE: usize = 0x018;
    pub const DSM_DIVIDER: usize = 0x020;
    pub const FEEDBACK_DIVIDER: usize = 0x024;
    pub const SYSTEM_MUXES: usize = 0x028;
    pub const CALIBRATION_SETTINGS: usize = 0x044;
    pub const BAND_SEL_CAL_SETTINGS_THREE: usize = 0x068;
    pub const FREQ_DETECT_SETTINGS_ONE: usize = 0x078;
    pub const PFILT: usize = 0x090;
    pub const IFILT: usize = 0x094;
    pub const OUTDIV: usize = 0x0a8;
    /// `PLL_OUTDIV_RATE` (`dsi_phy_7nm.xml`) - the VCO post-divider the vendor's
    /// clock tree owns (`pll_out_div` in `pll_7nm_register`). 2 bits, power-of-two.
    pub const PLL_OUTDIV_RATE: usize = 0x154;
    pub const CORE_OVERRIDE: usize = 0x0b8;
    pub const CORE_INPUT_OVERRIDE: usize = 0x0bc;
    pub const PLL_DIGITAL_TIMERS_TWO: usize = 0x0c8;
    pub const DECIMAL_DIV_START_1: usize = 0x0e0;
    pub const FRAC_DIV_START_LOW_1: usize = 0x0e4;
    pub const FRAC_DIV_START_MID_1: usize = 0x0e8;
    pub const FRAC_DIV_START_HIGH_1: usize = 0x0ec;
    pub const PLL_LOCKDET_RATE_1: usize = 0x158;
    pub const PLL_PROP_GAIN_RATE_1: usize = 0x160;
    pub const PLL_BAND_SEL_RATE_1: usize = 0x168;
    pub const PLL_INT_GAIN_IFILT_BAND_1: usize = 0x170;
    pub const PLL_FL_INT_GAIN_PFILT_BAND_1: usize = 0x178;
    pub const PLL_LOCK_OVERRIDE: usize = 0x190;
    pub const PLL_LOCK_DELAY: usize = 0x194;
    pub const VCO_CONFIG_1: usize = 0x240;
    pub const CLOCK_INVERTERS_1: usize = 0x248;
    pub const CMODE_1: usize = 0x250;
    pub const ANALOG_CONTROLS_FIVE_1: usize = 0x258;
    pub const PERF_OPTIMIZE: usize = 0x260;
    /// `PLL_COMMON_STATUS_ONE` - bit 0 is the lock indication
    /// (`dsi_pll_7nm_lock_status`, `dsi_phy_7nm.c:378-396`).
    pub const COMMON_STATUS_ONE: usize = 0x1b0;
}

/// Fractional divider width (`dsi_phy_7nm.c:42`).
pub const FRAC_BITS: u32 = 18;

/// Values the V4.1 D-PHY branch needs at our bit rate (`dsi_phy_7nm.c:1023-1028,
/// 1071-1079`). `less_than_1500_mhz` is true for 1.054 Gbps/lane.
pub const V4_1_LESS_THAN_1500MHZ: bool = true;

/// The PLL divider and inverter values for a given VCO rate
/// (`dsi_pll_calc_dec_frac`, `dsi_phy_7nm.c:127-199`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PllDividers {
    pub dec: u64,
    pub frac: u64,
    pub clock_inverters: u32,
}

/// Pure port of the PLL divider math. `vco_rate` is the VCO frequency in Hz.
pub fn pll_calc_dec_frac(vco_rate: u64) -> PllDividers {
    let fref = VCO_REF_CLK_RATE;
    let divider = fref * 2;
    let multiplier: u64 = 1 << FRAC_BITS;
    let dec_multiple = (vco_rate as u128 * multiplier as u128 / divider as u128) as u64;
    let dec = dec_multiple / multiplier;
    let frac = dec_multiple % multiplier;
    // V4.1 branch (`:176-184`).
    let clock_inverters = if vco_rate <= 1_000_000_000 {
        0xa0
    } else if vco_rate <= 2_500_000_000 {
        0x20
    } else if vco_rate <= 3_020_000_000 {
        0x00
    } else {
        0x40
    };
    PllDividers {
        dec,
        frac,
        clock_inverters,
    }
}

/// `analog_controls_five_1` / `vco_config_1` for the hzindep table
/// (`dsi_phy_7nm.c:262-292`).
pub fn pll_hzindep_values(vco_rate: u64) -> (u32, u32) {
    let mut analog_controls_five_1 = 0x01;
    if vco_rate >= 3_100_000_000 {
        analog_controls_five_1 = 0x03;
    }
    let vco_config_1 = if vco_rate < 1_520_000_000 {
        0x08
    } else if vco_rate < 2_990_000_000 {
        0x01
    } else {
        0x00
    };
    (analog_controls_five_1, vco_config_1)
}

/// `S_DIV_ROUND_UP` (Linux `linux/math.h`): round *away from zero*.
/// `(n + d - 1) / d` for positive operands, which is what the callers use.
#[inline]
pub const fn s_div_round_up(n: i64, d: i64) -> i64 {
    if n >= 0 { (n + d - 1) / d } else { n / d }
}

/// `linear_inter()` (`dsi_phy.c:17-28`).
///
/// `percent` is in hundredths; when `even` is set the result is forced even by
/// rounding down, and the result is never below `min_result`.
#[inline]
pub fn linear_inter(tmax: i32, tmin: i32, percent: i32, min_result: i32, even: bool) -> i32 {
    let v = s_div_round_up(((tmax - tmin) * percent) as i64, 100) as i32 + tmin;
    if even && (v & 1) != 0 {
        core::cmp::max(min_result, v - 1)
    } else {
        core::cmp::max(min_result, v)
    }
}

/// Lane timing values programmed into `CMN.TIMING_CTRL_1..8` and 12/13
/// (`dsi_phy_7nm.c:1155-1185`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DphyTiming {
    pub clk_zero: u32,
    pub clk_prepare: u32,
    pub clk_trail: u32,
    pub hs_exit: u32,
    pub hs_zero: u32,
    pub hs_prepare: u32,
    pub hs_trail: u32,
    pub hs_rqst: u32,
    pub clk_pre: u32,
    pub clk_post: u32,
}

/// `DIV_ROUND_UP` for positive operands.
#[inline]
const fn div_round_up(n: i64, d: i64) -> i64 {
    (n + d - 1) / d
}

/// Port of `msm_dsi_dphy_timing_calc_v4()` (`dsi_phy.c:373-461`).
///
/// Pure arithmetic over the requested bit clock and escape clock, so it is
/// verifiable on the host. Returns `None` when either rate is zero, matching the
/// driver's `-EINVAL`.
pub fn dphy_timing_calc_v4(bitclk_rate: u64, escclk_rate: u64) -> Option<DphyTiming> {
    if bitclk_rate == 0 || escclk_rate == 0 {
        return None;
    }
    let coeff: i64 = 1000;
    // mult_frac(NSEC_PER_MSEC, coeff, bit_rate / 1000)
    let ui: i64 = (1_000_000 * coeff) / (bitclk_rate as i64 / 1000);
    let ui_x8: i64 = ui << 3;
    let hb_en: i64 = 0;
    let mut t = DphyTiming::default();

    // clk_prepare
    let tmin = core::cmp::max(s_div_round_up(38 * coeff, ui_x8), 0);
    let tmax = core::cmp::max((95 * coeff) / ui_x8, 0);
    t.clk_prepare = linear_inter(tmax as i32, tmin as i32, 50, 0, false) as u32;

    // clk_zero
    let temp = 300 * coeff - ((t.clk_prepare as i64) << 3) * ui;
    let tmin = s_div_round_up(temp, ui_x8) - 1;
    let tmax = if tmin > 255 { 511 } else { 255 };
    t.clk_zero = linear_inter(tmax as i32, tmin as i32, 2, 0, false) as u32;

    // clk_trail
    let tmin = div_round_up(60 * coeff + 3 * ui, ui_x8);
    let temp = 105 * coeff + 12 * ui - 20 * coeff;
    let tmax = (temp + 3 * ui) / ui_x8;
    t.clk_trail = linear_inter(tmax as i32, tmin as i32, 30, 0, false) as u32;

    // hs_prepare
    let tmin = core::cmp::max(s_div_round_up(40 * coeff + 4 * ui, ui_x8), 0);
    let tmax = core::cmp::max((85 * coeff + 6 * ui) / ui_x8, 0);
    t.hs_prepare = linear_inter(tmax as i32, tmin as i32, 50, 0, false) as u32;

    // hs_zero
    let temp = 145 * coeff + 10 * ui - ((t.hs_prepare as i64) << 3) * ui;
    let tmin = s_div_round_up(temp, ui_x8) - 1;
    t.hs_zero = linear_inter(255, tmin as i32, 10, 0, false) as u32;

    // hs_trail
    let tmin = div_round_up(60 * coeff + 4 * ui, ui_x8) - 1;
    let temp = 105 * coeff + 12 * ui - 20 * coeff;
    let tmax = (temp / ui_x8) - 1;
    t.hs_trail = linear_inter(tmax as i32, tmin as i32, 30, 0, false) as u32;

    // hs_rqst
    let temp = 50 * coeff + ((hb_en << 2) - 8) * ui;
    t.hs_rqst = s_div_round_up(temp, ui_x8) as u32;

    // hs_exit
    let tmin = div_round_up(100 * coeff, ui_x8) - 1;
    t.hs_exit = linear_inter(255, tmin as i32, 10, 0, false) as u32;

    // clk_post
    let temp = 60 * coeff + 52 * ui + ((t.hs_trail as i64) + 1) * ui_x8;
    let tmin = div_round_up(temp, 16 * ui) - 1;
    t.clk_post = linear_inter(255, tmin as i32, 5, 0, false) as u32;

    // clk_pre (note: the driver uses a 1.25% margin here, not linear_inter)
    let temp = 52 * coeff + ((t.clk_prepare + t.clk_zero + 1) as i64) * ui_x8 + 54 * coeff;
    let tmin = div_round_up(temp, 16 * ui) - 1;
    let tmax = 255i64;
    t.clk_pre = (div_round_up((tmax - tmin) * 125, 10_000) + tmin) as u32;

    Some(t)
}

/// Hardware side of the PHY. Gated to the target so the math above stays
/// host-testable.
#[cfg(target_arch = "aarch64")]
pub mod hw {
    use super::{DphyTiming, PllDividers, cmn, lane, pll};

    #[inline]
    fn wr(base: usize, off: usize, value: u32) {
        unsafe { core::ptr::write_volatile((base + off) as *mut u32, value) };
    }

    #[inline]
    fn rd(base: usize, off: usize) -> u32 {
        unsafe { core::ptr::read_volatile((base + off) as *const u32) }
    }

    /// `dsi_pll_enable_pll_bias()` / `disable_pll_bias()` (`dsi_phy_7nm.c:398-442`).
    fn pll_bias(enable: bool) {
        let base = super::PHY_CMN_BASE;
        let p = super::PHY_PLL_BASE;
        let mut data = rd(base, cmn::CTRL_0);
        if enable {
            data |= 1 << 0; // PLL_SHUTDOWNB
            wr(base, cmn::CTRL_0, data);
            wr(p, pll::SYSTEM_MUXES, 0xc0);
        } else {
            data &= !(1 << 0);
            wr(p, pll::SYSTEM_MUXES, 0);
            wr(base, cmn::CTRL_0, data);
        }
        crate::timer::delay_us(1);
    }

    /// `dsi_pll_commit()` (`dsi_phy_7nm.c:326-345`).
    fn pll_commit(d: &PllDividers) {
        let p = super::PHY_PLL_BASE;
        wr(p, pll::CORE_INPUT_OVERRIDE, 0x12);
        wr(p, pll::DECIMAL_DIV_START_1, d.dec as u32);
        wr(p, pll::FRAC_DIV_START_LOW_1, (d.frac & 0xff) as u32);
        wr(
            p,
            pll::FRAC_DIV_START_MID_1,
            ((d.frac & 0xff00) >> 8) as u32,
        );
        wr(
            p,
            pll::FRAC_DIV_START_HIGH_1,
            ((d.frac & 0x3_0000) >> 16) as u32,
        );
        wr(p, pll::PLL_LOCKDET_RATE_1, 0x40);
        wr(p, pll::PLL_LOCK_DELAY, 0x06);
        wr(p, pll::CMODE_1, 0x10); // D-PHY
        wr(p, pll::CLOCK_INVERTERS_1, d.clock_inverters);
    }

    /// `dsi_pll_config_hzindep_reg()` (`dsi_phy_7nm.c:262-324`).
    fn pll_hzindep(vco_rate: u64) {
        let p = super::PHY_PLL_BASE;
        let (ac5_1, vco_cfg_1) = super::pll_hzindep_values(vco_rate);
        wr(p, pll::ANALOG_CONTROLS_FIVE_1, ac5_1);
        wr(p, pll::VCO_CONFIG_1, vco_cfg_1);
        wr(p, pll::ANALOG_CONTROLS_FIVE, 0x01);
        wr(p, pll::ANALOG_CONTROLS_TWO, 0x03);
        wr(p, pll::ANALOG_CONTROLS_THREE, 0x00);
        wr(p, pll::DSM_DIVIDER, 0x00);
        wr(p, pll::FEEDBACK_DIVIDER, 0x4e);
        wr(p, pll::CALIBRATION_SETTINGS, 0x40);
        wr(p, pll::BAND_SEL_CAL_SETTINGS_THREE, 0xba);
        wr(p, pll::FREQ_DETECT_SETTINGS_ONE, 0x0c);
        wr(p, pll::OUTDIV, 0x00);
        wr(p, pll::CORE_OVERRIDE, 0x00);
        wr(p, pll::PLL_DIGITAL_TIMERS_TWO, 0x08);
        wr(p, pll::PLL_PROP_GAIN_RATE_1, 0x0a);
        wr(p, pll::PLL_BAND_SEL_RATE_1, 0xc0);
        wr(p, pll::PLL_INT_GAIN_IFILT_BAND_1, 0x84);
        wr(p, pll::PLL_INT_GAIN_IFILT_BAND_1, 0x82);
        wr(p, pll::PLL_FL_INT_GAIN_PFILT_BAND_1, 0x4c);
        wr(p, pll::PLL_LOCK_OVERRIDE, 0x80);
        wr(p, pll::PFILT, 0x29);
        wr(p, pll::PFILT, 0x2f);
        wr(p, pll::IFILT, 0x2a);
        wr(p, pll::IFILT, 0x3f);
        wr(p, pll::PERF_OPTIMIZE, 0x22);
    }

    /// `dsi_pll_phy_dig_reset()` (`dsi_phy_7nm.c:481-492`).
    fn pll_phy_dig_reset() {
        let base = super::PHY_CMN_BASE;
        wr(base, cmn::GLBL_DIGTOP_SPARE4, 1);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        wr(base, cmn::GLBL_DIGTOP_SPARE4, 0);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    }

    /// `dsi_pll_enable_global_clk()` (`dsi_phy_7nm.c:473-479`).
    fn pll_enable_global_clk() {
        let base = super::PHY_CMN_BASE;
        wr(base, cmn::CTRL_3, 0x04);
        let cfg1 = rd(base, cmn::CLK_CFG1);
        wr(base, cmn::CLK_CFG1, cfg1 | (1 << 5) | (1 << 4));
    }

    /// Configure and start the PLL, then wait for lock.
    ///
    /// Sequence: `dsi_pll_7nm_vco_set_rate()` (`:347-376`) followed by
    /// `dsi_pll_7nm_vco_prepare()` (`:494-540`). Returns false if the PLL never
    /// reports lock within 5 ms.
    pub fn pll_start(vco_rate: u64) -> bool {
        let base = super::PHY_CMN_BASE;
        let p = super::PHY_PLL_BASE;

        // set_rate: bias on, program, bias off.
        pll_bias(true);
        let d = super::pll_calc_dec_frac(vco_rate);
        pll_commit(&d);
        pll_hzindep(vco_rate);
        pll_bias(false);

        // prepare: bias on, start, wait lock, digital reset, global clk.
        pll_bias(true);
        wr(base, cmn::PLL_CNTRL, 1);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        let mut locked = false;
        for _ in 0..50 {
            // `dsi_pll_7nm_lock_status`: COMMON_STATUS_ONE bit 0.
            if rd(p, pll::COMMON_STATUS_ONE) & 1 != 0 {
                locked = true;
                break;
            }
            crate::timer::delay_us(100);
        }

        pll_phy_dig_reset();
        pll_enable_global_clk();
        wr(base, cmn::RBUF_CTRL, 0x1);
        locked
    }

    /// The 35-step PHY enable sequence (`dsi_phy_7nm.c:1001-1188`) for V4.1 at
    /// `less_than_1500_mhz`, plus the lane settings.
    pub fn phy_enable(timing: &DphyTiming, lane_cfg: &[[u8; 4]; 5]) {
        let base = super::PHY_CMN_BASE;
        let lb = super::PHY_LANE_BASE;
        let p = super::PHY_PLL_BASE;

        // 1. wait for REFGEN ready: PHY_STATUS bit 0.
        for _ in 0..200 {
            if rd(base, cmn::PHY_STATUS) & 1 != 0 {
                break;
            }
            crate::timer::delay_us(5);
        }

        // 2-4.
        wr(base, cmn::CTRL_0, (1 << 4) | (1 << 0)); // DIGTOP_PWRDN_B | PLL_SHUTDOWNB
        wr(base, cmn::PLL_CNTRL, 0x00);
        wr(base, cmn::RBUF_CTRL, 0x00);
        // 5. CTRL_4 only for minor_ver 2 parts.
        if (rd(base, cmn::REVISION_ID0) & 0xf0) == 0x20 {
            wr(base, cmn::CTRL_4, 0x04);
        }
        // 6-7.
        wr(base, cmn::LANE_CFG0, 0x21);
        wr(base, cmn::LANE_CFG1, 0x84);
        // 8-9. LDO on.
        wr(base, cmn::VREG_CTRL_0, 0x53);
        wr(base, cmn::VREG_CTRL_1, 0x5c);
        // 10-16.
        wr(base, cmn::CTRL_3, 0x00);
        wr(base, cmn::GLBL_STR_SWI_CAL_SEL_CTRL, 0x00);
        wr(base, cmn::GLBL_HSTX_STR_CTRL_0, 0x88);
        wr(base, cmn::GLBL_PEMPH_CTRL_0, 0x00);
        wr(base, cmn::GLBL_RESCODE_OFFSET_TOP_CTRL, 0x3d);
        wr(base, cmn::GLBL_RESCODE_OFFSET_BOT_CTRL, 0x39);
        wr(base, cmn::GLBL_LPTX_STR_CTRL, 0x55);
        // 17-19.
        wr(base, cmn::CTRL_0, 0x7f);
        wr(base, cmn::LANE_CTRL0, 0x1f);
        wr(base, cmn::CTRL_2, 0x40);

        // 20. `dsi_7nm_set_usecase()` (`:691-720`): select the PLL source. For a
        // standalone PHY (a single DSI, which is our case) `data = 0`, so
        // CLK_CFG1.BITCLK_SEL is cleared - i.e. the internal PLL is selected. The
        // bootloader may have left this field set to the external PLL, so writing
        // it explicitly matters.
        let cfg1 = rd(base, cmn::CLK_CFG1);
        wr(base, cmn::CLK_CFG1, cfg1 & !(0x3 << 2));

        // PLL post-dividers. The vendor never writes these from driver code because
        // its clock framework owns them (`pll_7nm_register`, `dsi_phy_7nm.c:754-830`):
        //   pll_out_div = VCO / OUT_DIV      (PLL_OUTDIV_RATE, 2 bits, power-of-two)
        //   pll_bit     = pll_out_div / BIT_DIV (CMN.CLK_CFG0 [3:0], ONE-BASED)
        // With VCO = 2 x bit rate, OUT_DIV = 2 and BIT_DIV = 1 give
        // pll_bit = the bit rate, which is what the timing registers above assume.
        wr(p, pll::PLL_OUTDIV_RATE, 0x1);
        wr(base, cmn::CLK_CFG0, 0x1);

        // 21-34. D-PHY timing registers.
        wr(base, cmn::TIMING_CTRL_0, 0x00);
        wr(base, cmn::TIMING_CTRL_1, timing.clk_zero);
        wr(base, cmn::TIMING_CTRL_2, timing.clk_prepare);
        wr(base, cmn::TIMING_CTRL_3, timing.clk_trail);
        wr(base, cmn::TIMING_CTRL_4, timing.hs_exit);
        wr(base, cmn::TIMING_CTRL_5, timing.hs_zero);
        wr(base, cmn::TIMING_CTRL_6, timing.hs_prepare);
        wr(base, cmn::TIMING_CTRL_7, timing.hs_trail);
        wr(base, cmn::TIMING_CTRL_8, timing.hs_rqst);
        wr(base, cmn::TIMING_CTRL_9, 0x02);
        wr(base, cmn::TIMING_CTRL_10, 0x04);
        wr(base, cmn::TIMING_CTRL_11, 0x00);
        wr(base, cmn::TIMING_CTRL_12, timing.clk_pre);
        wr(base, cmn::TIMING_CTRL_13, timing.clk_post);

        // 35. Lane settings (`:927-958`) with the DT's per-lane values.
        for i in 0..5usize {
            let o = i * lane::STRIDE;
            wr(lb, o + lane::LPRX_CTRL, 0);
            wr(lb, o + lane::PIN_SWAP, 0);
        }
        wr(lb, lane::LPRX_CTRL, 0x3); // logical lane 0
        for i in 0..5usize {
            let o = i * lane::STRIDE;
            wr(lb, o + lane::CFG0, lane_cfg[i][0] as u32);
            wr(lb, o + lane::CFG1, lane_cfg[i][1] as u32);
            wr(lb, o + lane::CFG2, lane_cfg[i][2] as u32);
            wr(lb, o + lane::TX_DCTRL, lane_cfg[i][3] as u32);
        }
    }

    /// True when the PHY PLL currently reports lock (`PLL_COMMON_STATUS_ONE` bit 0).
    pub fn pll_locked() -> bool {
        rd(super::PHY_PLL_BASE, pll::COMMON_STATUS_ONE) & 1 != 0
    }

    /// Snapshot of the PHY lane status registers (`LANE_STATUS0` at 0x148,
    /// `LANE_STATUS1` at 0x14c). Non-zero means the lane block is reporting state,
    /// which is the bisection question "do the lanes come up at all".
    pub fn lane_status() -> (u32, u32) {
        (
            rd(super::PHY_CMN_BASE, cmn::LANE_STATUS0),
            rd(super::PHY_CMN_BASE, cmn::LANE_STATUS1),
        )
    }

    /// Read back the PHY PLL control register for diagnostics.
    pub fn pll_control() -> u32 {
        rd(super::PHY_CMN_BASE, cmn::PLL_CNTRL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::panel;

    /// The panel's own DT timings table is the independent ground truth:
    /// `qcom,mdss-dsi-panel-phy-timings = [00 23 09 09 26 24 09 09 06 02 04 00 1D 19]`
    /// (`dsi-panel-sofef01-1080p-cmd.dtsi:135-138`). If the bit rate derivation
    /// and this port are right, the computed values must reappear in that table.
    #[test]
    fn v4_timing_reproduces_panel_dt_table() {
        let bit = (panel::H_TOTAL as u64)
            * (panel::V_TOTAL as u64)
            * (panel::PANEL_FRAMERATE_HZ as u64)
            * (panel::PANEL_BPP as u64)
            / (panel::DSI_LANES as u64);
        assert_eq!(bit, 1_053_861_840, "per-lane bit rate for sofef01");

        let t = dphy_timing_calc_v4(bit, VCO_REF_CLK_RATE).expect("timing");
        let dt = panel::PHY_TIMINGS;

        // Every value below appears verbatim in the DT table.
        assert_eq!(t.clk_zero, dt[1] as u32, "clk_zero");
        assert_eq!(t.clk_prepare, dt[2] as u32, "clk_prepare");
        assert_eq!(t.hs_exit, dt[4] as u32, "hs_exit");
        assert_eq!(t.hs_zero, dt[5] as u32, "hs_zero");
        assert_eq!(t.hs_trail, dt[6] as u32, "hs_trail");
        assert_eq!(t.hs_prepare, dt[7] as u32, "hs_prepare");
        assert_eq!(t.hs_rqst, dt[8] as u32, "hs_rqst");
        assert_eq!(t.clk_post, dt[13] as u32, "clk_post");
    }

    #[test]
    fn v4_timing_rejects_zero_rates() {
        assert!(dphy_timing_calc_v4(0, VCO_REF_CLK_RATE).is_none());
        assert!(dphy_timing_calc_v4(1_053_861_840, 0).is_none());
    }
}
