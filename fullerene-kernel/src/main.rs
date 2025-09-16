#![no_std]
#![no_main]

mod vga;
mod serial;

use vga::vga_init;
use serial::{serial_init, serial_log};

#[unsafe(no_mangle)]
pub extern "C" fn efi_main() -> ! {
    vga_init();
    serial_init();
    vga::log("Entering _start");
    vga::log("Initializing memory");
    vga::log("Initializing drivers");
    serial_log("Hello Serial!\n");
    loop {}
}
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    serial_log("PANIC!\n");
    loop {}
}
