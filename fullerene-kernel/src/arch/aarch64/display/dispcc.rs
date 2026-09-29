//! Display clock controller (`dispcc`, Lito) - branch enables for the DSI path.
//!
//! Source: `tmp/display-src/vq_dispcc-lito.c` (bramble 4.19 qpr1). Offsets are
//! the driver's `.enable_reg`/`.halt_reg` values; the DT gives the controller
//! base as `disp_cc_base = 0x0af08000` (`lito-sde.dtsi:446-447`).
//!
//! Why this exists: the DSI controller's DT `clocks` list names six dispcc
//! clocks, and the bootloader handoff is known to leave clock domains gated (the
//! USB work proved the same class of problem for the USB branches). Skipping this
//! was the first attempt's most likely failure: the DSI registers were programmed
//! but the panel saw nothing.
//!
//! The byte and pixel clocks are sourced from the DSI PHY PLL, so their *rates*
//! come from the PLL dividers, not from this module - only the branch gates and
//! the AHB/MDP bus clocks are handled here.

/// The dispcc register block: `qcom,dispcc@af00000` with
/// `reg = <0xaf00000 0x20000>` (`lito.dtsi:1626-1628`).
///
/// Note: the DSI controller node's second reg entry is also called
/// `disp_cc_base` but is only **4 bytes** (`lito-sde.dtsi:446-447`), i.e. a single
/// status register, not the clock block. Using it as a base silently writes to
/// unrelated registers inside the same 128 KiB window.
pub const DISP_CC_BASE: usize = 0x0af0_0000;

/// GCC base (same block the USB clock layer uses).
pub const GCC_BASE: usize = 0x0010_0000;

/// GCC display branches. The MDSS node's `clocks` list (`lito-sde.dtsi:17-28`)
/// names three of these as `gcc_iface`, `gcc_bus` and `gcc_nrt_bus`, and the
/// dispcc node itself is fed by `GCC_DISP_AHB_CLK`. Without them the dispcc
/// register block is inaccessible and every write to it is silently dropped -
/// which is exactly the symptom the first hardware runs showed.
pub mod gcc_branch {
    pub const DISP_AHB_CLK: usize = 0xb00c;
    pub const QMIP_DISP_AHB_CLK: usize = 0xb020;
    pub const DISP_HF_AXI_CLK: usize = 0xb030;
    pub const DISP_SF_AXI_CLK: usize = 0xb034;
    pub const DISP_XO_CLK: usize = 0xb040;
}

/// GCC block resets (`gcc_lito_resets[]`, `vq_gcc-lito.c:2591-2610`).
pub mod gcc_reset {
    /// `GCC_MMSS_BCR` - the multimedia subsystem block reset, which covers the
    /// whole display subsystem. A block held in reset ignores writes, so if the
    /// handoff left this asserted every DSI/MDSS register write would be dropped
    /// silently - exactly the symptom the hardware runs show.
    ///
    /// qcom reset semantics (`drivers/clk/qcom/reset.c`): BIT(0) asserts, 0
    /// deasserts.
    pub const MMSS_BCR: usize = 0xb000;
}

/// Branch enable registers (`.enable_reg`, bit 0; `BRANCH_HALT` polls the same
/// offset for the branch to reach the requested state).
pub mod branch {
    pub const MDSS_PCLK0_CLK: usize = 0x2004;
    pub const MDSS_MDP_CLK: usize = 0x200c;
    pub const MDSS_ROT_CLK: usize = 0x2014;
    pub const MDSS_MDP_LUT_CLK: usize = 0x201c;
    pub const MDSS_VSYNC_CLK: usize = 0x2024;
    pub const MDSS_BYTE0_CLK: usize = 0x2028;
    pub const MDSS_BYTE0_INTF_CLK: usize = 0x202c;
    pub const MDSS_ESC0_CLK: usize = 0x2038;
    pub const MDSS_AHB_CLK: usize = 0x2080;
}

/// RCG command registers (`.cmd_rcgr`); the config word is at `+0x4`, and BIT(0)
/// of the command register commits an update.
pub mod rcg {
    pub const MDSS_PCLK0_CLK_SRC: usize = 0x2098;
    pub const MDSS_BYTE0_CLK_SRC: usize = 0x2110;
    pub const MDSS_ESC0_CLK_SRC: usize = 0x2148;

    /// Source-select value for the DSI0 PHY PLL output in every map the DSI path
    /// uses (`disp_cc_parent_map_0` for byte/esc, `_4` for pixel; both place the
    /// DSI0 PLL output at index 1).
    pub const SRC_DSI0_PHY_PLL: u32 = 1;
}

/// Branches the DSI path needs, in the order the DSI controller's DT lists them
/// followed by the MDSS bus clocks.
pub const NEEDED: &[(&str, usize)] = &[
    ("ahb", branch::MDSS_AHB_CLK),
    ("mdp", branch::MDSS_MDP_CLK),
    ("byte0", branch::MDSS_BYTE0_CLK),
    ("byte0_intf", branch::MDSS_BYTE0_INTF_CLK),
    ("pclk0", branch::MDSS_PCLK0_CLK),
    ("esc0", branch::MDSS_ESC0_CLK),
];

#[cfg(target_arch = "aarch64")]
pub mod hw {
    use super::{DISP_CC_BASE, branch};

    #[inline]
    fn rd(off: usize) -> u32 {
        unsafe { core::ptr::read_volatile((DISP_CC_BASE + off) as *const u32) }
    }

    #[inline]
    fn wr(off: usize, value: u32) {
        unsafe { core::ptr::write_volatile((DISP_CC_BASE + off) as *mut u32, value) };
    }

    /// Deassert the MMSS block reset. Must run before any display register write:
    /// a block held in reset silently drops them.
    ///
    /// Returns the register value *before* the change, so a run can report whether
    /// the handoff had left the subsystem in reset.
    pub fn deassert_mmss_reset() -> u32 {
        let addr = super::GCC_BASE + super::gcc_reset::MMSS_BCR;
        let before = unsafe { core::ptr::read_volatile(addr as *const u32) };
        // BIT(0) asserts; clear it to release the block.
        unsafe { core::ptr::write_volatile(addr as *mut u32, before & !1) };
        crate::timer::delay_us(10);
        before
    }

    /// Point the DSI byte/pixel/escape clock RCGs at the PHY PLL and set their
    /// dividers. Without this the branches are enabled but clocked from whatever
    /// the bootloader left selected, so the DSI link has no usable clock.
    ///
    /// RCG2 layout: `cmd_rcgr` is the command register (BIT(0) commits), the config
    /// word sits at `+0x4` with `SRC_DIV` in bits [4:0] and `SRC_SEL` in [10:8].
    /// Dividers, derived from the PLL clock tree (`pll_7nm_register`,
    /// `dsi_phy_7nm.c:754-830`) rather than guessed:
    ///
    /// * `pll_bit` = the per-lane bit rate = 1,053,861,840 Hz (VCO 2,107,723,680
    ///   divided by OUT_DIV 2 and BIT_DIV 1).
    /// * byte0 parent is `dsi0_phy_pll_out_byteclk` = `pll_bit / 8` = 131.7 MHz, so
    ///   the byte divider is 1.
    /// * pclk0 parent is `pclk_mux`, which selects `pll_bit` (DSICLK_SEL = 0) at
    ///   1053.9 MHz, so reaching the 175.6 MHz pixel clock needs a divider of 6.
    /// * esc0 parent is the same byte clock, and 131.7 / 7 = 18.8 MHz sits inside
    ///   the MIPI 20..5 MHz escape band.
    pub fn configure_dsi_rcgs() -> bool {
        let mut ok = true;
        for (cmd, divider) in [
            (super::rcg::MDSS_BYTE0_CLK_SRC, 1u32),
            (super::rcg::MDSS_PCLK0_CLK_SRC, 6),
            (super::rcg::MDSS_ESC0_CLK_SRC, 7),
        ] {
            let cfg = super::DISP_CC_BASE + cmd + 0x4;
            let mut value = unsafe { core::ptr::read_volatile(cfg as *const u32) };
            value &= !(0x1f | (0x7 << 8));
            value |= divider & 0x1f;
            value |= (super::rcg::SRC_DSI0_PHY_PLL << 8) & (0x7 << 8);
            unsafe { core::ptr::write_volatile(cfg as *mut u32, value) };

            let command = super::DISP_CC_BASE + cmd;
            let v = unsafe { core::ptr::read_volatile(command as *const u32) } | 1;
            unsafe { core::ptr::write_volatile(command as *mut u32, v) };
            let mut committed = false;
            for _ in 0..1000 {
                if unsafe { core::ptr::read_volatile(command as *const u32) } & 1 == 0 {
                    committed = true;
                    break;
                }
                crate::timer::delay_us(1);
            }
            ok &= committed;
        }
        ok
    }

    /// Enable the GCC display branches. These must come FIRST: the dispcc block is
    /// fed by `GCC_DISP_AHB_CLK`, so until that is running the dispcc writes below
    /// go nowhere.
    pub fn enable_gcc_display_clocks() -> usize {
        let mut ok = 0usize;
        for off in [
            super::gcc_branch::DISP_AHB_CLK,
            super::gcc_branch::DISP_HF_AXI_CLK,
            super::gcc_branch::DISP_SF_AXI_CLK,
            super::gcc_branch::QMIP_DISP_AHB_CLK,
            super::gcc_branch::DISP_XO_CLK,
        ] {
            let addr = super::GCC_BASE + off;
            let value = unsafe { core::ptr::read_volatile(addr as *const u32) } | 1;
            unsafe { core::ptr::write_volatile(addr as *mut u32, value) };
            for _ in 0..1000 {
                if unsafe { core::ptr::read_volatile(addr as *const u32) } & 1 != 0 {
                    ok += 1;
                    break;
                }
                crate::timer::delay_us(1);
            }
        }
        ok
    }

    /// Enable one branch and wait for it to come out of halt.
    fn enable_branch(off: usize) -> bool {
        let value = rd(off) | 1;
        wr(off, value);
        for _ in 0..1000 {
            if rd(off) & 1 != 0 {
                return true;
            }
            crate::timer::delay_us(1);
        }
        false
    }

    /// Enable every branch the DSI path needs. Returns the number that reported
    /// enabled, so a run can tell "clocks were already on" from "we had to raise
    /// them" only through the count of failures.
    pub fn enable_dsi_clocks() -> usize {
        let mut ok = 0usize;
        for (_, off) in super::NEEDED {
            if enable_branch(*off) {
                ok += 1;
            }
        }
        ok
    }

    /// Read back a branch register for diagnostics.
    pub fn branch_state(name: &str) -> u32 {
        super::NEEDED
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, off)| rd(*off))
            .unwrap_or(0)
    }

    /// Snapshot of all needed branches, for the diagnostic path.
    pub fn snapshot() -> [u32; 6] {
        let mut out = [0u32; 6];
        for (i, (_, off)) in super::NEEDED.iter().enumerate() {
            out[i] = rd(*off);
        }
        out
    }

    /// True when every needed branch is already enabled.
    pub fn all_enabled() -> bool {
        super::NEEDED.iter().all(|(_, off)| rd(*off) & 1 != 0)
    }

    /// `branch` is re-exported so callers can name offsets without importing both
    /// modules.
    pub use super::branch as branches;
    #[allow(unused_imports)]
    use branch as _branch;
}
