//! Interrupt Descriptor Table (IDT) management
//!
//! This module provides IDT initialization and handler setup.

use super::apic::{KEYBOARD_INTERRUPT_INDEX, MOUSE_INTERRUPT_INDEX, TIMER_INTERRUPT_INDEX};
use super::exceptions::{breakpoint_handler, double_fault_handler, page_fault_handler};
use super::input::{keyboard_handler, mouse_handler, timer_handler};
use crate::gdt;
use lazy_static::lazy_static;
use x86_64::structures::idt::InterruptDescriptorTable;

/// Macro to reduce repetitive IDT handler setup
macro_rules! setup_idt_handler {
    ($idt:expr, $field:ident, $handler:ident) => {
        $idt.$field.set_handler_fn($handler);
    };
}

/// Macro to set up IDT handler with optional stack index based on target OS
macro_rules! setup_idt_handler_with_stack {
    ($idt:expr, $field:ident, $handler:ident, $stack_index:expr) => {
        #[cfg(not(target_os = "uefi"))]
        unsafe {
            $idt.$field
                .set_handler_fn($handler)
                .set_stack_index($stack_index);
        }
        #[cfg(target_os = "uefi")]
        {
            $idt.$field.set_handler_fn($handler);
        }
    };
}

// Global Interrupt Descriptor Table
lazy_static! {
    pub static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        // Set up CPU exception handlers
        setup_idt_handler!(idt, breakpoint, breakpoint_handler);
        setup_idt_handler!(idt, page_fault, page_fault_handler);
        setup_idt_handler_with_stack!(idt, double_fault, double_fault_handler, gdt::DOUBLE_FAULT_IST_INDEX);

        // Set up hardware interrupt handlers - disabled in UEFI mode due to CPU state issues
        #[cfg(not(target_os = "uefi"))]
        unsafe {
            idt[TIMER_INTERRUPT_INDEX as u8]
                .set_handler_fn(timer_handler)
                .set_stack_index(gdt::TIMER_IST_INDEX);
            idt[KEYBOARD_INTERRUPT_INDEX as u8].set_handler_fn(keyboard_handler);
            idt[MOUSE_INTERRUPT_INDEX as u8].set_handler_fn(mouse_handler);
        }
        #[cfg(target_os = "uefi")]
        {
            // Skip setting up hardware interrupts in UEFI mode to avoid invalid opcode errors
            // UEFI has its own interrupt handling setup
        }

        idt
    };
}

/// Initialize IDT (load it into the CPU)
pub fn init() {
    petroleum::serial::serial_log(format_args!("About to load IDT...\n"));
    IDT.load();
    petroleum::serial::serial_log(format_args!("IDT.load() completed, about to log completion...\n"));
    petroleum::serial::serial_log(format_args!("IDT loaded with exception handlers.\n"));
}
