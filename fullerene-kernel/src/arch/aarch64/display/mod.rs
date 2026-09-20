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

pub mod panel;

/// True once the display has been brought up far enough to accept pixels.
pub const fn is_ready() -> bool {
    false
}
