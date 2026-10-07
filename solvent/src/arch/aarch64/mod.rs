//! aarch64 (ANE) runtime pieces.
//!
//! The phone has no timestamp counter equivalent wired into the runtime yet,
//! so time is driven by the kernel-side desktop timer: the driver bumps
//! [`ANE_TICKS`] on every tick and the frame scheduler reads it here. The
//! input stack stays on the x86 side until the ANE input bring-up lands, so
//! its pump hooks and scancode mappings exist here as inert stubs and the
//! shared desktop callers compile unchanged.

use core::sync::atomic::{AtomicU64, Ordering};

/// Tick counter advanced by the kernel's desktop timer driver.
pub static ANE_TICKS: AtomicU64 = AtomicU64::new(0);

/// Monotonic timestamp source: the ANE tick counter.
#[inline]
pub fn now_tsc() -> u64 {
    ANE_TICKS.load(Ordering::Relaxed)
}

// ── Input pump hooks (no keyboard/mouse stack on the ANE yet) ─────────

/// No keyboard to pump.
pub fn poll_keyboard() {}

/// No mouse state to pump.
pub fn poll_mouse_state() {}

/// No cursor-paint instrumentation.
pub fn record_cursor_paint() {}

/// No PS/2 scancodes on the ANE yet.
pub fn scancode_to_ascii(_scancode: u8) -> u8 {
    0
}

/// No PS/2 scancodes on the ANE yet.
pub fn scancode_to_resonance_keycode(_scancode: u8) -> resonance::KeyCode {
    resonance::KeyCode::Escape
}

/// No Wi-Fi UI on the ANE yet.
pub fn handle_network_action(
    _rt: &mut crate::RuntimeState,
    _action: &lattice::desktop::DesktopAction,
) {
}
