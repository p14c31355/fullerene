//! Bramble panel (`sofef01`) description - permanent hardware facts.
//!
//! Everything here is extracted from the vendor device tree, not guessed:
//! `tmp/qpr1-msm/arch/arm64/boot/dts/google/lito-bramble-display.dtsi` and
//! `dsi-panel-sofef01-1080p-cmd.dtsi`. See `docs/DISPLAY_BRINGUP.md` for the
//! full extraction table and the staged plan.
//!
//! The panel is a 1080x2340 Samsung AMOLED in **command mode**: the host pushes
//! frames into panel RAM and the panel refreshes on its own tear signal (TE), so
//! there is no free-running video stream to program - the DPU must be told to
//! issue a software-triggered commit per frame.

/// Active panel on Bramble.
pub const PANEL_NAME: &str = "sofef01";

/// Panel geometry.
pub const PANEL_WIDTH: u32 = 1080;
pub const PANEL_HEIGHT: u32 = 2340;

/// Bits per pixel (RGB888, `qcom,mdss-dsi-bpp = <24>`).
pub const PANEL_BPP: u32 = 24;

/// DSI data lanes (`lane-0..3-state`, `lane_map_0123`).
pub const DSI_LANES: u32 = 4;

/// Refresh rate (`qcom,mdss-dsi-panel-framerate = <60>`).
pub const PANEL_FRAMERATE_HZ: u32 = 60;

/// Horizontal timing: front porch, back porch, pulse width (`:77-79`).
pub const H_FRONT_PORCH: u32 = 32;
pub const H_BACK_PORCH: u32 = 98;
pub const H_PULSE_WIDTH: u32 = 32;

/// Vertical timing: back porch, front porch, pulse width (`:82-84`).
pub const V_BACK_PORCH: u32 = 8;
pub const V_FRONT_PORCH: u32 = 8;
pub const V_PULSE_WIDTH: u32 = 1;

/// Derived line/frame totals used by the DSI timing generator.
pub const H_TOTAL: u32 = PANEL_WIDTH + H_FRONT_PORCH + H_BACK_PORCH + H_PULSE_WIDTH; // 1242
pub const V_TOTAL: u32 = PANEL_HEIGHT + V_BACK_PORCH + V_FRONT_PORCH + V_PULSE_WIDTH; // 2357

/// Panel reset sequence (`qcom,mdss-dsi-reset-sequence = <0 10>, <1 10>`):
/// drive the reset line low for 10 ms, then high for 10 ms.
pub const RESET_LOW_MS: u32 = 10;
pub const RESET_HIGH_MS: u32 = 10;

/// Panel reset GPIO: `pm8150l_gpios 8` (`lito-bramble-display.dtsi:24`).
pub const RESET_PMIC_GPIO: u32 = 8;

/// Tear-signal GPIO: `tlmm 10` (`lito-bramble-display.dtsi:23`).
pub const TE_GPIO: u32 = 10;

/// DSI PHY timing table (`qcom,mdss-dsi-panel-phy-timings`). The vendor PHY
/// driver decodes this into its lane timing registers; the port must do the same
/// rather than invent values.
pub const PHY_TIMINGS: [u8; 14] = [
    0x00, 0x23, 0x09, 0x09, 0x26, 0x24, 0x09, 0x09, 0x06, 0x02, 0x04, 0x00, 0x1D, 0x19,
];

/// One DSI command in the vendor DT's packed form:
/// `type last vc ack wait_ms_hi wait_ms_lo dlen_hi dlen_lo payload...`.
///
/// * `type` - 0x05 DCS short write with one parameter, 0x15 DCS short write with
///   none, 0x39 DCS long write.
/// * `last` - 1 marks the final command of a sequence.
/// * `wait` - delay in milliseconds after the command completes.
pub struct DsiCommand {
    pub kind: u8,
    pub last: bool,
    pub wait_ms: u16,
    pub payload: &'static [u8],
}

/// Panel power-on sequence (`qcom,mdss-dsi-on-command`, panel DT `:89-108`).
/// Sent in LP mode. Ends with DCS 0x29 (display on).
pub const PANEL_ON: &[DsiCommand] = &[
    // Sleep out, then wait 10 ms before anything else.
    DsiCommand {
        kind: 0x05,
        last: false,
        wait_ms: 10,
        payload: &[0x11],
    },
    // Manufacturer command set unlock (F0 5A 5A).
    DsiCommand {
        kind: 0x39,
        last: false,
        wait_ms: 0,
        payload: &[0xF0, 0x5A, 0x5A],
    },
    // Dimming frames = 8.
    DsiCommand {
        kind: 0x15,
        last: false,
        wait_ms: 0,
        payload: &[0xB0, 0x07],
    },
    DsiCommand {
        kind: 0x15,
        last: false,
        wait_ms: 0,
        payload: &[0xB7, 0x08],
    },
    // Tear signal on.
    DsiCommand {
        kind: 0x15,
        last: false,
        wait_ms: 0,
        payload: &[0x35, 0x00],
    },
    // MIC setting.
    DsiCommand {
        kind: 0x39,
        last: false,
        wait_ms: 0,
        payload: &[0xEB, 0x17, 0x41, 0x92, 0x0E, 0x10, 0x86, 0x5A],
    },
    // Manufacturer command set lock (F0 A5 A5).
    DsiCommand {
        kind: 0x39,
        last: false,
        wait_ms: 0,
        payload: &[0xF0, 0xA5, 0xA5],
    },
    // CASET: column address 0..1079.
    DsiCommand {
        kind: 0x39,
        last: false,
        wait_ms: 0,
        payload: &[0x2A, 0x00, 0x00, 0x04, 0x37],
    },
    // PASET: page address 0..2339.
    DsiCommand {
        kind: 0x39,
        last: false,
        wait_ms: 0,
        payload: &[0x2B, 0x00, 0x00, 0x09, 0x23],
    },
    // Brightness, then wait 110 ms.
    DsiCommand {
        kind: 0x15,
        last: false,
        wait_ms: 110,
        payload: &[0x53, 0x28],
    },
    // Display on - the last command of the sequence.
    DsiCommand {
        kind: 0x05,
        last: true,
        wait_ms: 0,
        payload: &[0x29],
    },
];

/// Panel power-off sequence (`qcom,mdss-dsi-off-command`, panel DT `:109-116`).
pub const PANEL_OFF: &[DsiCommand] = &[
    // Display off, wait 20 ms.
    DsiCommand {
        kind: 0x05,
        last: false,
        wait_ms: 20,
        payload: &[0x28],
    },
    // Sleep in.
    DsiCommand {
        kind: 0x05,
        last: false,
        wait_ms: 0,
        payload: &[0x10],
    },
    DsiCommand {
        kind: 0x39,
        last: false,
        wait_ms: 0,
        payload: &[0xF0, 0x5A, 0x5A],
    },
    DsiCommand {
        kind: 0x15,
        last: false,
        wait_ms: 0,
        payload: &[0xB0, 0x05],
    },
    DsiCommand {
        kind: 0x15,
        last: false,
        wait_ms: 0,
        payload: &[0xF4, 0x01],
    },
    // Lock, wait 120 ms.
    DsiCommand {
        kind: 0x39,
        last: true,
        wait_ms: 120,
        payload: &[0xF0, 0xA5, 0xA5],
    },
];

/// Sequence that leaves low-power mode (`qcom,mdss-dsi-nolp-command`).
pub const PANEL_NOLP: &[DsiCommand] = &[
    DsiCommand {
        kind: 0x05,
        last: false,
        wait_ms: 0,
        payload: &[0x28],
    },
    DsiCommand {
        kind: 0x39,
        last: false,
        wait_ms: 0,
        payload: &[0xF0, 0x5A, 0x5A],
    },
    // HLPM off.
    DsiCommand {
        kind: 0x15,
        last: false,
        wait_ms: 1,
        payload: &[0x53, 0x28],
    },
    DsiCommand {
        kind: 0x39,
        last: false,
        wait_ms: 34,
        payload: &[0xF0, 0xA5, 0xA5],
    },
    DsiCommand {
        kind: 0x05,
        last: true,
        wait_ms: 0,
        payload: &[0x29],
    },
];

/// Sequence that enters low-power mode (`qcom,mdss-dsi-lp1-command`).
pub const PANEL_LP1: &[DsiCommand] = &[
    DsiCommand {
        kind: 0x05,
        last: false,
        wait_ms: 0,
        payload: &[0x28],
    },
    DsiCommand {
        kind: 0x39,
        last: false,
        wait_ms: 0,
        payload: &[0xF0, 0x5A, 0x5A],
    },
    // HLPM on at 50 nit.
    DsiCommand {
        kind: 0x15,
        last: false,
        wait_ms: 1,
        payload: &[0x53, 0x22],
    },
    DsiCommand {
        kind: 0x39,
        last: true,
        wait_ms: 17,
        payload: &[0xF0, 0xA5, 0xA5],
    },
];
