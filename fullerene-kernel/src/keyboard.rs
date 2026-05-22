//! Keyboard input driver (re-exported from Nitrogen)
//!
//! This module re-exports the PS/2 keyboard driver from the Nitrogen crate.
//! The actual driver logic (scancode-to-ASCII conversion, modifier tracking,
//! input buffering) lives in `nitrogen::ps2::keyboard`.

pub use nitrogen::ps2::keyboard::*;

/// Legacy alias for backwards compatibility.
///
/// The original `crate::keyboard::init()` is now `init_keyboard()`
/// in the Nitrogen driver.  This alias lets existing callers (shell,
/// uefi_main) continue to work without modification.
pub use nitrogen::ps2::keyboard::init_keyboard as init;
