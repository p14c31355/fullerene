//! Touch-input surface for the ANE.
//!
//! The x86 tree talks to the Intel LPSS DesignWare I2C-HID touchpad; the ANE
//! has no I2C-HID host wired yet, so the same names exist here with no
//! hardware behind them. The report types come from the shared `hid` decoder,
//! so the desktop input path compiles and can be exercised before the bus
//! exists.

use crate::hid::{MAX_TOUCH_CONTACTS, TouchContact, TouchpadReport};

/// Snapshot of one decoded touchpad report (shape mirrors the x86 side).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchpadInput {
    pub report: TouchpadReport,
    pub x_min: i32,
    pub x_max: i32,
    pub y_min: i32,
    pub y_max: i32,
    /// Relative-motion delta when the report carried one.
    pub relative: Option<(i16, i16)>,
    /// Absolute digitizer contact slots.
    pub contacts: [Option<TouchContact>; MAX_TOUCH_CONTACTS],
    pub contact_count: u8,
}

/// No I2C-HID host on the ANE yet: nothing to service, nothing pending.
pub fn service_input() -> bool {
    false
}

/// Compatibility alias for the x86 polling entry point.
pub fn poll_input() {}

/// No I2C-HID controller on the ANE yet, so there is no status to report.
pub fn status_snapshot() -> Option<(alloc::string::String, alloc::string::String)> {
    None
}
