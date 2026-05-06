//! Interrupt Descriptor Table (IDT) management
//!
//! This module provides IDT initialization and handler setup.

use super::apic::{KEYBOARD_INTERRUPT_INDEX, MOUSE_INTERRUPT_INDEX, TIMER_INTERRUPT_INDEX};
use super::exceptions::{breakpoint_handler, double_fault_handler, page_fault_handler};
use super::input::{keyboard_handler, mouse_handler, timer_handler};
use x86_64::structures::idt::InterruptDescriptorTable;

// Global Interrupt Descriptor Table
pub static mut IDT: InterruptDescriptorTable = InterruptDescriptorTable::new();

/// Initialize IDT (load it into the CPU)
#[allow(static_mut_refs)]
pub fn init() {
    unsafe { petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [idt::init] start\n") };
    
    unsafe {
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [idt::init] configuring IDT\n");
        let idt = &mut IDT;

        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [idt::init] setting up exceptions\n");
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        
        // Avoid IST for now to minimize risk of Triple Fault
        idt.double_fault.set_handler_fn(double_fault_handler);
        
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [idt::init] setting up hardware interrupts\n");
        idt[TIMER_INTERRUPT_INDEX as u8].set_handler_fn(timer_handler);
        idt[KEYBOARD_INTERRUPT_INDEX as u8].set_handler_fn(keyboard_handler);
        idt[MOUSE_INTERRUPT_INDEX as u8].set_handler_fn(mouse_handler);

        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [idt::init] loading IDT\n");
        idt.load();
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [idt::init] IDT loaded successfully\n");
    }
    unsafe { petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [idt::init] done\n") };
}
