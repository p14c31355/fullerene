//! Interrupt handling module for Fullerene OS
//!
//! This module provides interrupt handling capabilities including
//! IDT management, APIC setup, legacy PIC disable, exception handling,
//! hardware interrupts, and system call mechanism.

pub mod apic;
pub mod exceptions;
pub mod idt;
pub mod input;
pub mod syscall;

use core::sync::atomic::AtomicU64;
use x86_64::instructions::interrupts;

// Global tick counter for timing (lock-free atomic)
pub static TICK_COUNTER: AtomicU64 = AtomicU64::new(0);

// Re-export public functions and structures
pub use exceptions::{
    alignment_check_handler, bound_range_exceeded_handler, breakpoint_handler,
    coprocessor_segment_overrun_handler, debug_handler, device_not_available_handler,
    divide_error_handler, double_fault_handler, general_protection_fault_handler,
    hv_injection_exception_handler, invalid_opcode_handler, invalid_tss_handler,
    machine_check_handler, nmi_handler, overflow_handler, page_fault_handler,
    security_exception_handler, segment_not_present_handler, stack_segment_fault_handler,
    virtualization_handler, vmm_communication_exception_handler,
};
pub use idt::init;
pub use input::{MOUSE_STATE, keyboard_handler, mouse_handler, timer_handler};
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
