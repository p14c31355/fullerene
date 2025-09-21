// fullerene-kernel/src/interrupts.rs

use crate::{gdt, serial, vga};
use core::fmt::Write;
use lazy_static::lazy_static;
use pc_keyboard::{DecodedKey, HandleControl, Keyboard, ScancodeSet1, layouts};
use pic8259::ChainedPics;
use spin::Mutex;
use x86_64::instructions::port::Port;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

// Programmable Interrupt Controller (PIC) configuration
pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

// Use a Mutex to wrap ChainedPics for safe access in a multi-threaded context.
pub static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

lazy_static! {
    // The Interrupt Descriptor Table (IDT)
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        // Set up handlers for CPU exceptions
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }

        // Set up handlers for hardware interrupts
        unsafe {
            idt[InterruptIndex::Timer.as_u8()].set_handler_fn(timer_interrupt_handler);
            idt[InterruptIndex::Keyboard.as_u8()].set_handler_fn(keyboard_interrupt_handler);
        }

        idt
    };
}

// Enum to represent hardware interrupt indices, with an offset for the PICs.
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard,
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }
}

// Initialize the Interrupt Descriptor Table (IDT).
pub fn init_idt() {
    IDT.load();
}

// Timer interrupt handler.
pub extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    // Notify the PIC that the interrupt has been handled.
    // This is crucial to prevent the PIC from repeatedly raising the same interrupt.
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
}

// Keyboard interrupt handler.
pub extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    lazy_static! {
        static ref KEYBOARD: Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>> =
            Mutex::new(Keyboard::new(
                ScancodeSet1::new(),
                layouts::Us104Key,
                HandleControl::Ignore
            ));
    }

    let mut keyboard = KEYBOARD.lock();
    let mut port = Port::new(0x60);

    let scancode: u8 = unsafe { port.read() };
    if let Ok(Some(key_event)) = keyboard.add_byte(scancode)
        && let Some(key) = keyboard.process_keyevent(key_event)
    {
        let mut serial_writer = serial::SERIAL1.lock();
        let vga_lock = vga::VGA_BUFFER.get();
        match key {
            DecodedKey::Unicode(character) => {
                let _ = serial_writer.write_char(character);
                if let Some(vga) = vga_lock {
                    let mut vga_writer = vga.lock();
                    vga_writer.write_byte(character as u8);
                    vga_writer.update_cursor();
                }
            }
            DecodedKey::RawKey(key) => {
                let _ = write!(serial_writer, "{:?}", key);
            }
        }
    }

    // Notify the PIC that the interrupt has been handled.
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}

// Exception handlers (not directly related to the fix, but included for completeness)
pub extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    let mut writer = serial::SERIAL1.lock();
    writeln!(
        writer,
        "\nEXCEPTION: BREAKPOINT\n{:#?}",
        stack_frame
    )
    .ok();
}

pub extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    let mut writer = serial::SERIAL1.lock();
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
