//! x86_64 runtime pieces: the input stack, Wi-Fi UI glue, and services.
//!
//! Only compiled on x86 targets; solvent/lib.rs globs this tree so the
//! historic paths (`solvent::input_loop::...`) are unchanged.

pub mod input_loop;
pub mod network_manager;

// Input glue used flat across the desktop (`crate::poll_keyboard`, ...).
pub use input_loop::{poll_keyboard, poll_mouse_state};
pub use input_loop::{record_cursor_paint, scancode_to_ascii, scancode_to_resonance_keycode};

/// Route a desktop menu action to the Wi-Fi UI.
pub fn handle_network_action(
    rt: &mut crate::RuntimeState,
    action: &lattice::desktop::DesktopAction,
) {
    network_manager::handle_network_action(rt, action);
}

/// Monotonic timestamp source: the CPU timestamp counter.
#[inline]
pub fn now_tsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}
