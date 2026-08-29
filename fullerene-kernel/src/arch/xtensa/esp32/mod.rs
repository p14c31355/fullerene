//! Fullerene's ESP32/Xtensa kernel port.
//!
//! The SoC layer is shared across ESP32 boards. The XH-32S/ESP32-2432S028
//! carrier profile is separate so a later board cannot silently inherit its
//! pins or calibration. This profile is a single-address-space Fullerene
//! profile; tasks are scheduled kernel tasks, not isolated processes.

pub mod board;
pub mod interrupts;
pub mod memory;
pub mod platform;
pub mod runtime;
pub mod scheduler;
pub mod time;

/// Native Rust entry called after Bellows establishes the stack.
#[unsafe(no_mangle)]
pub extern "C" fn rust_entry() -> ! {
    memory::init_heap();
    runtime::boot()
}
