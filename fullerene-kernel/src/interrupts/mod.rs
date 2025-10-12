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

use core::arch::asm;
use lazy_static::lazy_static;
use spin::Mutex;
use x86_64::registers::control::Cr3;

// Global tick counter for timing
lazy_static! {
    pub static ref TICK_COUNTER: Mutex<u64> = Mutex::new(0);
}

// Re-export public functions and structures
pub use apic::{init_apic, APIC};
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
    unsafe { asm!("cli") };
}

/// Enable interrupts
pub fn enable_interrupts() {
    unsafe { asm!("sti") };
}

/// Wait for interrupt (hlt instruction)
pub fn hlt_loop() -> ! {
    loop {
        unsafe { asm!("hlt") };
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
    unsafe { asm!("int3") };
}
