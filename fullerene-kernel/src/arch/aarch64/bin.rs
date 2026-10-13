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

// v319: the reenumerate quiet-wait reads the EP1 conversation counter
// through the crate root (ane_usb_dev is a platform-level sibling).
pub(crate) fn ane_conversations() -> u32 {
    runtime::platform::ane_usb_dev::conversations_served()
}

pub(crate) fn ane_uptime_us() -> u64 {
    runtime::timer::uptime_us()
}

pub(crate) fn ane_tick_count() -> u32 {
    runtime::exceptions::tick_count()
}

/// v328: the solvent render stage probe - mirrors the stage byte into the
/// durable ring so any collection names the render's progress.
#[unsafe(no_mangle)]
pub extern "Rust" fn ane_stage_probe(stage: u8) {
    crate::blackbox::text_puts("RP: ");
    crate::blackbox::text_byte(stage);
    crate::blackbox::text_puts("\n");
}
