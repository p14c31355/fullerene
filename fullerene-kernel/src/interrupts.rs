// fullerene-kernel/src/interrupts.rs

use crate::gdt;
use petroleum::serial::SERIAL_PORT_WRITER as SERIAL1;
use core::fmt::Write;
use lazy_static::lazy_static;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

// Macro to reduce repetitive IDT handler setup
macro_rules! setup_idt_handler {
    ($idt:expr, $field:ident, $handler:ident) => {
        $idt.$field.set_handler_fn($handler);
    };
}

lazy_static! {
    // The Interrupt Descriptor Table (IDT)
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        // Set up handlers for CPU exceptions
        setup_idt_handler!(idt, breakpoint, breakpoint_handler);
        setup_idt_handler!(idt, page_fault, page_fault_handler);
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }

        // Hardware interrupts (PIC/APIC) removed for now, implement APIC later

        idt
    };
}

// Initialize the Interrupt Descriptor Table (IDT) only (PIC/APIC init later)
pub fn init() {
    IDT.load();
    // x86_64::instructions::interrupts::enable(); // Enable after APIC setup
}

// Exception handlers
pub extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    let mut writer = SERIAL1.lock();
    writeln!(writer, "\nEXCEPTION: BREAKPOINT\n{:#?}", stack_frame).ok();
}

pub extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    let mut writer = SERIAL1.lock();
    writeln!(
        writer,
        "\nEXCEPTION: PAGE FAULT\n{:#?}\nError Code: {:?}",
        stack_frame, error_code
    )
    .ok();
}

pub extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    panic!("\nEXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}
