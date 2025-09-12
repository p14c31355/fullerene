// fullerene/fullerene-kernel/src/main.rs
#![no_std]
#![no_main]

use core::fmt::Write;

use uefi::prelude::*;
use uefi::proto::console::text::Output;
use uefi::table::boot::{AllocateType, MemoryType};

mod vga;
use spin::once::Once;
use x86_64::instructions::port::{PortRead, PortWrite};

// OLD: Removed VgaBuffer struct and its implementation
// struct VgaBuffer { ... }
// impl VgaBuffer { ... }

// NEW: SerialPort struct for direct serial communication
struct SerialPort {
    data_port: u16,
    line_status_port: u16,
}

impl SerialPort {
    const COM1: u16 = 0x3F8;

    fn new() -> Self {
        let port = Self {
            data_port: Self::COM1,
            line_status_port: Self::COM1 + 5,
        };
        // Basic initialization for COM1 (115200 baud, 8N1)
        unsafe {
            // Disable interrupts
            PortWrite::write_to_port(port.data_port + 1, 0x00u8);
            // Enable DLAB (Divisor Latch Access Bit) to set baud rate
            PortWrite::write_to_port(port.data_port + 3, 0x80u8);
            // Set baud rate (divisor = 1 for 115200 baud if clock is 1.8432 MHz)
            PortWrite::write_to_port(port.data_port + 0, 0x01u8); // Low byte
            PortWrite::write_to_port(port.data_port + 1, 0x00u8); // High byte
            // Disable DLAB, set 8 data bits, no parity, 1 stop bit
            PortWrite::write_to_port(port.data_port + 3, 0x03u8);
            // Enable FIFO, clear them, with 14-byte threshold
            PortWrite::write_to_port(port.data_port + 2, 0xC7u8);
            // Enable IRQs, set RTS/DSR (Modem Control Register)
            PortWrite::write_to_port(port.data_port + 4, 0x0Bu8);
        }
        port
    }

    fn write_byte(&self, byte: u8) {
    unsafe {
        let mut lsr: u8;
        loop {
            lsr = PortRead::read_from_port(self.line_status_port);
            if lsr & 0x20 != 0 { break; }
        }
        PortWrite::write_to_port(self.data_port, byte);
    }
}

    fn write_string(&self, s: &str) {
        for byte in s.bytes() {
            self.write_byte(byte);
        }
    }
}

unsafe impl Send for SerialPort {}
unsafe impl Sync for SerialPort {}

// Replace VGA static with SERIAL static
static SERIAL: Once<SerialPort> = Once::new();

#[entry]
fn main(image_handle: uefi::Handle, mut system_table: SystemTable<Boot>) -> Status {
    uefi_services::init(&mut system_table).expect("Failed to initialize UEFI services.");

    // Clear screen and enable cursor are not directly available without TextOutput trait in scope.
    // Using uefi_services::println! for output.
    // stdout.clear_screen().expect("Failed to clear screen");
    // stdout.enable_cursor(true).expect("Failed to enable cursor");

    vga::vga_init();
    SERIAL.call_once(|| SerialPort::new());
    SERIAL
        .get()
        .unwrap()
        .write_string("Hello QEMU by fullerene!\n");

    uefi_services::println!("Hello UEFI from fullerene-kernel!");

    Status::SUCCESS
}
