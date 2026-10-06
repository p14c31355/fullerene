#![allow(dead_code)]

extern crate alloc;

#[path = "../src/arch/aarch64/fdt.rs"]
mod fdt;

/// `fdt.rs` emits the ANE bring-up step marks through
/// `entry::ane_step_byte`; the host harness runs the walker without the
/// boot entry, so the marks are no-ops here. Signature must stay in step
/// with `src/arch/aarch64/entry.rs`.
mod entry {
    pub(crate) unsafe fn ane_step_byte(_byte: u8) {}
}
