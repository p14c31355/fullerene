#![no_std]
#![no_main]

mod serial;
mod vga;

use serial::{serial_init, serial_log};
use vga::vga_init;

#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(_image_handle: usize, _system_table: *mut core::ffi::c_void) -> ! {
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
