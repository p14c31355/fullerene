#![no_std]
#![no_main]

mod vga;

use core::panic::PanicInfo;
use vga::vga_init;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    vga_init();

    loop {}
}
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}