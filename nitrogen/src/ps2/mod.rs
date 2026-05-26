//! PS/2 device drivers.
//!
//! This module provides drivers for PS/2 peripherals (mouse, keyboard, etc.)
//! using the underlying port I/O primitives available in Nitrogen.

pub mod keyboard;
pub mod keymap;
pub mod mouse;
