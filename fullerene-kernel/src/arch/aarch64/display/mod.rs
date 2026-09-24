//! Bramble display bring-up - permanent readout equipment for the handset.
//!
//! Why this module exists: every host-visible USB diagnostic on this device is
//! dead (inert CCS pulses, Run/Stop cycles that produce no re-attach, park-duration
//! gates dominated by the harness, `bramble-usb trace` needing an enumeration, and
//! no display backend at all). The one measurement the evidence archive still lists
//! as open - a post-attach readout of `EP0_SETUP_ARMED`, `SETUP_ARM_FAILURE_STAGE`,
//! `SOFFN`, the event ring and `DSTS`/`DCTL` - therefore needs an on-device
//! readout, and the panel is the only display this hardware has.
//!
//! The bootloader's framebuffer cannot be reused: its DPU is stopped and the panel
//! simply holds its last frame (proved by filling the whole 36 MB "Display
//! Reserved" region with no visible change, run `451168.0`). So the kernel must
//! bring the display up itself.
//!
//! Full extraction table, block versions and the staged plan:
//! `docs/DISPLAY_BRINGUP.md`.
//!
//! Staging (each stage independently verifiable on hardware):
//!   A. facts + skeleton (this module, `panel.rs`)          <- current
//!   B. display clocks and panel power/reset
//!   C. DSI PHY (`qcom,dsi-phy-v4.1`, 7nm) PLL and lanes
//!   D. DSI controller (`qcom,dsi-ctrl-hw-v2.4`) + panel init
//!   E. DPU scanout, then text rendering
//!   F. print the USB device-side state at the attach moment
//!
//! Rules: source before hypothesis (every sequence cites the vendor driver),
//! one stage at a time, and never perturb the USB path - keep display init behind
//! its own build flag until it is proven.

pub mod dispcc;
pub mod dpu;
pub mod dsi_ctrl;
pub mod dsi_phy;
pub mod panel;

/// True once the display has been brought up far enough to accept pixels.
pub const fn is_ready() -> bool {
    false
}

/// Why `bring_up()` stopped, for the on-device diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    PlbLockFailed,
    PhyEnabled,
    CtrlConfigured,
    /// The PM8150L GPIO reset of the panel failed (SPMI unavailable, or the write
    /// did not stick). This is also the bisection question "does the SPMI slave
    /// write path work at all".
    PanelResetFailed,
    /// The command engine never accepted a panel-init transfer (`TRIG_DMA` stayed
    /// set). This is the bisection question "does the command path work at all".
    CmdTxFailed,
    PanelInitSent,
    Done,
}

/// Send one panel sequence over the DSI command path, honouring the per-command
/// delays from the DT descriptor.
#[cfg(target_arch = "aarch64")]
fn send_sequence(cmds: &[panel::DsiCommand]) -> usize {
    let mut sent = 0usize;
    for cmd in cmds {
        let pkt = dsi_ctrl::Packet {
            dtype: cmd.kind,
            vc: 0,
            payload: cmd.payload,
        };
        if dsi_ctrl::hw::cmd_tx(&pkt) {
            sent += 1;
        }
        if cmd.wait_ms > 0 {
            crate::timer::delay_us(cmd.wait_ms as u64 * 1000);
        }
        let _ = cmd.last;
    }
    sent
}

/// MIPI DCS command bytes used by the direct write path.
pub const DCS_SET_COLUMN_ADDRESS: u8 = 0x2a;
pub const DCS_SET_PAGE_ADDRESS: u8 = 0x2b;
pub const DCS_WRITE_MEMORY_START: u8 = 0x2c;
pub const DCS_WRITE_MEMORY_CONTINUE: u8 = 0x3c;

/// A colour as the panel expects it. The panel's `qcom,mdss-dsi-color-order` is
/// `rgb_swap_rgb` and bpp is 24, so the DSI payload is R, G, B per pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const MAGENTA: Rgb = Rgb {
        r: 0xff,
        g: 0x00,
        b: 0xff,
    };
}

/// Fill a horizontal band of the panel with one colour, writing straight to the
/// panel's frame memory over DSI.
///
/// This deliberately bypasses the DPU: `sofef01` is a command-mode panel, so the
/// host can push pixels with `RAMWR` (DCS 0x2C) plus data, exactly the path the
/// panel's own init sequence uses for its non-pixel commands. That avoids porting
/// the Lito DPU catalog before anything can be seen.
///
/// `first_row`/`rows` select the band; `chunk_pixels` bounds each DSI long write.
/// Returns the number of chunks successfully transferred.
#[cfg(target_arch = "aarch64")]
pub fn fill_band(first_row: u16, rows: u16, color: Rgb, chunk_pixels: usize) -> usize {
    let mut ok = 0usize;
    let x1: u16 = (panel::PANEL_WIDTH - 1) as u16;
    let y0 = first_row;
    let y1 = first_row + rows - 1;
    let caset = [0u8, 0, (x1 >> 8) as u8, (x1 & 0xff) as u8];
    let paset = [
        (y0 >> 8) as u8,
        (y0 & 0xff) as u8,
        (y1 >> 8) as u8,
        (y1 & 0xff) as u8,
    ];
    if dsi_ctrl::hw::cmd_tx_raw(DCS_SET_COLUMN_ADDRESS, 0, &caset) {
        ok += 1;
    }
    if dsi_ctrl::hw::cmd_tx_raw(DCS_SET_PAGE_ADDRESS, 0, &paset) {
        ok += 1;
    }

    // Pixel payload, one long write at a time. The staging buffer is 1 KiB, so a
    // chunk carries at most ~338 RGB pixels.
    let total_pixels = panel::PANEL_WIDTH as usize * rows as usize;
    let mut done = 0usize;
    let mut first = true;
    while done < total_pixels {
        let n = core::cmp::min(chunk_pixels, total_pixels - done);
        let mut buf = [0u8; 16368];
        let bytes = n * 3;
        if bytes > buf.len() {
            break;
        }
        let mut i = 0;
        while i + 2 < bytes {
            buf[i] = color.r;
            buf[i + 1] = color.g;
            buf[i + 2] = color.b;
            i += 3;
        }
        let dtype = if first {
            first = false;
            DCS_WRITE_MEMORY_START
        } else {
            DCS_WRITE_MEMORY_CONTINUE
        };
        if dsi_ctrl::hw::cmd_tx_raw(dtype, 0, &buf[..bytes]) {
            ok += 1;
        }
        done += n;
    }
    ok
}

/// Bring the display up to the point where it can accept a frame.
///
/// Order follows the vendor driver: PHY PLL, PHY enable, controller config,
/// panel init. Clocks and panel power are deliberately not touched yet - the
/// bootloader left the panel initialised and holding its last frame, so this
/// tries to re-establish the link on top of that state first.
#[cfg(target_arch = "aarch64")]
pub fn bring_up() -> Stage {
    bring_up_variant(panel::Variant::Sofef01)
}

/// Reuse-only bring-up: send the panel commands and the frame without re-initialising
/// the link.
///
/// XBL initialises MDSS and DISPCC (both appear in `xbl_a.elf`; the DSI ctrl and PHY
/// base addresses appear nowhere in any bootloader image), which is why the handset
/// shows the bootloader logo. This port then re-programs the PLL, PHY, controller and
/// clocks from scratch on top of that working state - and every write lands while
/// nothing is displayed.
///
/// This is the same call that already worked once here: the DPU was never ported, the
/// bootloader's DPU configuration was reused instead. So this variant deliberately
/// touches no clock, no PLL, no PHY and no controller register: it sends the panel
/// sequence and the frame through whatever XBL left live.
#[cfg(target_arch = "aarch64")]
pub fn bring_up_reuse() -> Stage {
    bring_up_reuse_variant(panel::Variant::Sofef01)
}

/// Reuse-only bring-up for a chosen panel variant.
///
/// The variant matters here for a concrete reason: `lito-bramble.dtsi` names
/// `sofef01` as the default, but the *dev* overlay (`lito-bramble-dev.dtsi:23`) -
/// which is what a developer handset runs - overrides it to
/// `dsi_sofef00_sdc_1080p_cmd`. The two have never been tested in combination with
/// the no-re-initialisation path, and each half alone explains nothing.
#[cfg(target_arch = "aarch64")]
pub fn bring_up_reuse_variant(variant: panel::Variant) -> Stage {
    let sequence = match variant {
        panel::Variant::Sofef01 => panel::PANEL_ON,
        panel::Variant::Sofef00 => panel::PANEL_ON_SOFEF00,
    };
    unsafe {
        let _ = super::platform::bramble::reset_panel_gpio8();
    }
    let sent = send_sequence(sequence);
    if sent < sequence.len() {
        return Stage::CmdTxFailed;
    }
    Stage::PanelInitSent
}

/// Bring the display up for a chosen panel variant.
///
/// The DT carries three panel candidates and marks `sofef01` only as the default;
/// the vendor driver selects by reading the panel ID. A wrong pick would leave every
/// SoC-side block healthy (which the bisection confirmed) and the glass blank, so the
/// variant is selectable and one run per candidate settles which panel is present.
#[cfg(target_arch = "aarch64")]
pub fn bring_up_variant(variant: panel::Variant) -> Stage {
    let (height, v_total, sequence) = match variant {
        panel::Variant::Sofef01 => (panel::PANEL_HEIGHT, panel::V_TOTAL, panel::PANEL_ON),
        panel::Variant::Sofef00 => (
            panel::SOFEF00_HEIGHT,
            panel::SOFEF00_V_TOTAL,
            panel::PANEL_ON_SOFEF00,
        ),
    };
    let bitclk = (panel::H_TOTAL as u64)
        * (v_total as u64)
        * (panel::PANEL_FRAMERATE_HZ as u64)
        * (panel::PANEL_BPP as u64)
        / (panel::DSI_LANES as u64);
    // The 7nm D-PHY VCO runs at twice the per-lane bit clock.
    let vco = bitclk * 2;

    let timing = match dsi_phy::dphy_timing_calc_v4(bitclk, dsi_phy::VCO_REF_CLK_RATE) {
        Some(t) => t,
        None => return Stage::PlbLockFailed,
    };

    // Release the MMSS block reset first, then bring the display clocks up. A block
    // held in reset ignores register writes, and the handoff is known to leave
    // things gated/asserted - so this is the first thing that must be cleared.
    let _mmss_reset_before = dispcc::hw::deassert_mmss_reset();
    // Reset the panel before opening a new DSI session: a panel that went through the
    // bootloader's session can ignore a second one until it is reset. Uses PM8150L
    // GPIO 8 (the DT's reset line) via the SPMI transport.
    let panel_reset = unsafe { super::platform::bramble::reset_panel_gpio8() };
    if panel_reset.is_err() {
        return Stage::PanelResetFailed;
    }
    let _ = dispcc::hw::enable_gcc_display_clocks();
    let _ = dispcc::hw::enable_dsi_clocks();

    if !dsi_phy::hw::pll_start(vco) {
        return Stage::PlbLockFailed;
    }
    // The byte/pixel/escape RCGs must point at the PHY PLL, which only exists once
    // the PLL is running - so this comes after `pll_start` and before anything that
    // needs those clocks (the panel init is sent in LP mode and needs the escape
    // clock).
    let _ = dispcc::hw::configure_dsi_rcgs();
    dsi_phy::hw::phy_enable(&timing, &panel::LANE_CONFIG);

    dsi_ctrl::hw::sw_reset();
    let cfg = dsi_ctrl::CtrlConfig {
        lanes: panel::DSI_LANES,
        clk_post: timing.clk_post,
        clk_pre: timing.clk_pre,
        dlane_swap: 0,
        continuous_clock: false,
        eot_packet: false,
    };
    dsi_ctrl::hw::ctrl_config(&cfg);
    dsi_ctrl::hw::timing_setup(panel::PANEL_WIDTH, height, panel::PANEL_BPP);
    let _ = Stage::CtrlConfigured;

    let sent = send_sequence(sequence);
    if sent < sequence.len() {
        // At least one transfer never completed: the command engine did not take it.
        return Stage::CmdTxFailed;
    }
    Stage::Done
}
