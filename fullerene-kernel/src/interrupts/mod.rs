//! Interrupt handling module for Fullerene OS
//!
//! This module provides interrupt handling capabilities including
//! IDT management, APIC setup, legacy PIC disable, exception handling,
//! hardware interrupts, and system call mechanism.

pub mod apic;
pub mod exceptions;
pub mod idt;
pub mod input;
pub mod pic;
pub mod syscall;

use lazy_static::lazy_static;
use spin::Mutex;
use x86_64::instructions::interrupts;

// Global tick counter for timing
lazy_static! {
    pub static ref TICK_COUNTER: Mutex<u64> = Mutex::new(0);
}

// Re-export public functions and structures
pub use apic::{APIC, init_apic};
pub use exceptions::{handle_page_fault, page_fault_handler};
pub use idt::init;
pub use input::{KEYBOARD_QUEUE, MOUSE_STATE, keyboard_handler, mouse_handler, timer_handler};
pub use pic::disable_legacy_pic;
pub use syscall::setup_syscall;

/// Send End-Of-Interrupt to APIC
pub fn send_eoi() {
    crate::interrupts::apic::send_eoi();
}

/// Disable interrupts
pub fn disable_interrupts() {
    interrupts::disable();
}

/// Enable interrupts
pub fn enable_interrupts() {
    interrupts::enable();
}

/// Wait for interrupt (using pause for QEMU-friendliness instead of hlt)
pub fn hlt_loop() -> ! {
    loop {
        petroleum::cpu_pause();
    }
}

// Symbol exports for linking
#[unsafe(no_mangle)]
pub extern "C" fn syscall_entry() {
    syscall::syscall_entry();
}

// Panic button for debugging
#[cfg(debug_assertions)]
pub fn trigger_breakpoint() {
    interrupts::int3();
}
