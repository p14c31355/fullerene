//! Interrupt Descriptor Table (IDT) management
//!
//! This module provides IDT initialization and handler setup.

use super::apic::{KEYBOARD_INTERRUPT_INDEX, MOUSE_INTERRUPT_INDEX, TIMER_INTERRUPT_INDEX};
use super::exceptions::{breakpoint_handler, double_fault_handler, page_fault_handler};
use super::input::{keyboard_handler, mouse_handler, timer_handler};
use petroleum::mem_debug;
use x86_64::structures::idt::InterruptDescriptorTable;

// Global Interrupt Descriptor Table
pub static mut IDT: InterruptDescriptorTable = InterruptDescriptorTable::new();

/// Initialize IDT (load it into the CPU)
#[allow(static_mut_refs)]
pub fn init() {
    mem_debug!("IDT: Initializing\n");

    unsafe {
        let idt = &mut IDT;

        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        idt.double_fault.set_handler_fn(double_fault_handler);

        idt[TIMER_INTERRUPT_INDEX as u8].set_handler_fn(timer_handler);
        idt[KEYBOARD_INTERRUPT_INDEX as u8].set_handler_fn(keyboard_handler);
        idt[MOUSE_INTERRUPT_INDEX as u8].set_handler_fn(mouse_handler);

        mem_debug!("IDT: Loading IDT\n");
        idt.load();
    }

    mem_debug!("IDT: Initialized successfully\n");
}