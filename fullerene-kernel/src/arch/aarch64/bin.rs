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

// v315: the pump-side park recovery calls the desktop bring-up through the
// crate root (desktop is a private sibling module of the runtime).
pub(crate) fn ane_park_desktop() {
    runtime::desktop::PARK_RECOVERY_RUN.store(true, core::sync::atomic::Ordering::Relaxed);
    runtime::desktop::bring_up();
    // The tick-driven row flushes take the canvas from here; re-enable the
    // desktop ownership marker exactly as the boot-thread path does.
}
