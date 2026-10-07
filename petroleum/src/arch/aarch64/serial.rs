//! Console shim so the shared logging path compiles on the ANE.
//!
//! The real console is the durable blackbox ring, driven directly by the
//! kernel; `serial_log` is the one hook `common::logging` calls into.

/// Mirrors the x86 serial logger entry point, writing nothing.
pub fn serial_log(_args: core::fmt::Arguments<'_>) {}

/// Mirror of the serial `_print` hook (used by the handler macros on x86;
/// provided anyway so the name exists on both sides).
pub fn _print(_args: core::fmt::Arguments<'_>) {}
