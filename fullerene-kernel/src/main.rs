// fullerene/fullerene-kernel/src/main.rs
#![no_std]
#![no_main]

mod vga;

use vga::vga_init;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    vga_init();

    loop {}
}
