// fullerene-kernel/src/interrupts.rs

use crate::{gdt, serial, vga};
use core::fmt::Write;
use lazy_static::lazy_static;
use pc_keyboard::{DecodedKey, HandleControl, Keyboard, ScancodeSet1, layouts};
use pic8259::ChainedPics;
use spin::Mutex;
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
    vga::log("EXCEPTION: BREAKPOINT");
    let mut writer = vga::VGA_BUFFER.get().unwrap().lock();
    writer
        .write_fmt(format_args!("{:#?}", stack_frame))
        .unwrap();
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    vga::log("EXCEPTION: PAGE FAULT");
    let mut writer = vga::VGA_BUFFER.get().unwrap().lock();
    writer
        .write_fmt(format_args!("Accessed Address: {:?}", Cr2::read()))
        .unwrap();
    writer
        .write_fmt(format_args!("Error Code: {:?}", error_code))
        .unwrap();
    writer
        .write_fmt(format_args!("{:#?}", stack_frame))
        .unwrap();

    loop {}
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    panic!(
        "EXCEPTION: DOUBLE FAULT
{:#?}",
        stack_frame
    );
}

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

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8())
    };
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

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
    if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
        if let Some(key) = keyboard.process_keyevent(key_event) {
            let mut serial_writer = serial::SERIAL1.lock();
            match key {
                DecodedKey::Unicode(character) => {
                    let _ = serial_writer.write_char(character);
                }
                DecodedKey::RawKey(key) => {
                    let _ = serial_writer.write_fmt(format_args!("{:?}", key));
                }
            }
        }
    }
        let mut serial_writer = serial::SERIAL1.lock();
        match key {
            DecodedKey::Unicode(character) => {
                let _ = serial_writer.write_char(character);
            }
            DecodedKey::RawKey(key) => {
                let _ = serial_writer.write_fmt(format_args!("{:?}", key));
            }
        }
    }

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8())
    };
}
