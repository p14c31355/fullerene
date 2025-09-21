// fullerene-kernel/src/interrupts.rs

use crate::{gdt, serial};
use core::fmt::Write;
use lazy_static::lazy_static;
use pc_keyboard::{DecodedKey, HandleControl, Keyboard, ScancodeSet1, layouts};
use pic8259::ChainedPics;
use spin::Mutex;
use x86_64::instructions::port::Port;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

pub static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt[InterruptIndex::Timer.as_u8()].set_handler_fn(timer_interrupt_handler);
        idt[InterruptIndex::Keyboard.as_u8()].set_handler_fn(keyboard_interrupt_handler);
        idt
    };
}

pub fn init_idt() {
    IDT.load();
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    serial::serial_log("EXCEPTION: BREAKPOINT");
    let _ = core::fmt::write(
        &mut *serial::SERIAL1.lock(),
        format_args!("{:#?}\n", stack_frame),
    );
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    serial::serial_log("EXCEPTION: PAGE FAULT");
    let _ = core::fmt::write(
        &mut *serial::SERIAL1.lock(),
        format_args!("Error Code: {:#?}\n", error_code),
    );
    let _ = core::fmt::write(
        &mut *serial::SERIAL1.lock(),
        format_args!("{:#?}\n", stack_frame),
    );
    loop {}
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    serial::serial_log("EXCEPTION: DOUBLE FAULT");
    let _ = core::fmt::write(
        &mut *serial::SERIAL1.lock(),
        format_args!("{:#?}\n", stack_frame),
    );
    panic!();
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
#[allow(dead_code)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard,
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }
}

// Global counter for timer interrupts
static mut TIMER_INTERRUPT_COUNT: u64 = 0;

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    // Increment the counter to prove the handler is running
    unsafe {
        TIMER_INTERRUPT_COUNT += 1;
        // Optionally, print the count to show progress, but only if it's safe.
        // For now, let's remove it to be safe.
        // serial::serial_log(&alloc::format!("TIMER IRQ0 fired! count: {}", TIMER_INTERRUPT_COUNT));
    }

    // Notify the PIC that the interrupt has been handled.
    // This is crucial to prevent the PIC from re-asserting the interrupt line.
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8())
    };
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
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
        match key {
            DecodedKey::Unicode(character) => {
                let _ = serial_writer.write_char(character);
            }
            DecodedKey::RawKey(key) => {
                let _ = write!(serial_writer, "{:?}", key);
            }
        }
    }

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8())
    };
}
