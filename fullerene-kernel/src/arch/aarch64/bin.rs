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
#[path = "../../cdc_acm.rs"]
mod cdc_acm;
#[path = "../../blackbox.rs"]
mod blackbox;

pub(crate) use runtime::timer;
