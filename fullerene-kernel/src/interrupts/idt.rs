//! Interrupt Descriptor Table (IDT) management
//!
//! This module provides IDT initialization and handler setup.

use super::apic::{KEYBOARD_INTERRUPT_INDEX, MOUSE_INTERRUPT_INDEX, TIMER_INTERRUPT_INDEX};
use super::exceptions::{breakpoint_handler, double_fault_handler, page_fault_handler};
use super::input::{keyboard_handler, mouse_handler, timer_handler};
use lazy_static::lazy_static;
use x86_64::structures::idt::InterruptDescriptorTable;

/// Macro to reduce repetitive IDT handler setup
macro_rules! setup_idt_handler {
    ($idt:expr, $field:ident, $handler:ident) => {
        $idt.$field.set_handler_fn($handler);
    };
}

// Global Interrupt Descriptor Table
lazy_static! {
    pub static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        // Set up CPU exception handlers
        setup_idt_handler!(idt, breakpoint, breakpoint_handler);
        setup_idt_handler!(idt, page_fault, page_fault_handler);



        setup_idt_handler!(idt, double_fault, double_fault_handler);

        // Set up hardware interrupt handlers
        unsafe {
            idt[TIMER_INTERRUPT_INDEX as u8].set_handler_fn(timer_handler);
            idt[KEYBOARD_INTERRUPT_INDEX as u8].set_handler_fn(keyboard_handler);
            idt[MOUSE_INTERRUPT_INDEX as u8].set_handler_fn(mouse_handler);
        }

        idt
    };
}

/// Initialize IDT (load it into the CPU)
pub fn init() {
    petroleum::serial::serial_log(format_args!("About to load IDT...\n"));
    IDT.load();
    petroleum::serial::serial_log(format_args!(
        "IDT.load() completed, about to log completion...\n"
    ));
    petroleum::serial::serial_log(format_args!("IDT loaded with exception handlers.\n"));
}
