#![no_std]
#![no_main]

use core::ptr::{read_volatile, write_volatile};

const UART_BASE: usize = 0x0900_0000;
const UART_DR: usize = UART_BASE;
const UART_FR: usize = UART_BASE + 0x18;
const UART_IBRD: usize = UART_BASE + 0x24;
const UART_FBRD: usize = UART_BASE + 0x28;
const UART_LCR_H: usize = UART_BASE + 0x2c;
const UART_CR: usize = UART_BASE + 0x30;

const UART_FR_TXFF: u32 = 1 << 5;
const UART_CR_UARTEN: u32 = 1 << 0;
const UART_CR_TXE: u32 = 1 << 8;
const UART_CR_RXE: u32 = 1 << 9;

/// Initialize QEMU virt's PL011 at 115200 8N1.
fn uart_init() {
    unsafe {
        write_volatile(UART_CR as *mut u32, 0);
        // QEMU virt supplies a 24 MHz PL011 clock.
        write_volatile(UART_IBRD as *mut u32, 13);
        write_volatile(UART_FBRD as *mut u32, 1);
        write_volatile(UART_LCR_H as *mut u32, 0b11 << 5);
        write_volatile(
            UART_CR as *mut u32,
            UART_CR_UARTEN | UART_CR_TXE | UART_CR_RXE,
        );
    }
}

fn uart_putc(byte: u8) {
    unsafe {
        while read_volatile(UART_FR as *const u32) & UART_FR_TXFF != 0 {
            core::hint::spin_loop();
        }
        write_volatile(UART_DR as *mut u32, byte as u32);
    }
}

fn uart_puts(message: &str) {
    for byte in message.bytes() {
        if byte == b'\n' {
            uart_putc(b'\r');
        }
        uart_putc(byte);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    uart_init();
    uart_puts("hello from fullerene aarch64\n");
    uart_puts("platform: qemu-virt, uart: pl011\n");

    loop {
        unsafe { core::arch::asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    uart_puts("fullerene aarch64 panic\n");
    loop {
        unsafe { core::arch::asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}
