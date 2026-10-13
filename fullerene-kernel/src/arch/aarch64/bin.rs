#![feature(alloc_error_handler)]
#![no_std]
#![no_main]

extern crate alloc;

#[path = "main.rs"]
mod runtime;

// `arch/aarch64/main.rs` (now `runtime`) refers to the black box as
// `crate::blackbox`, so this crate root has to declare it too. The file lives at
// the package's `src/` root, hence the relative path from here.
#[path = "../../adb_wire.rs"]
mod adb_wire;
#[path = "../../blackbox.rs"]
mod blackbox;
#[path = "../../cdc_acm.rs"]
mod cdc_acm;

pub(crate) use runtime::timer;

// v310: the ane platform's beat-gated reenumerate FSM reads the tick count.
// `tick_count` lives under `runtime::exceptions`; the platform tree reaches
// it through the crate root.
pub(crate) fn ane_tick_count() -> u32 {
    runtime::exceptions::tick_count()
}
