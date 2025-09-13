// fullerene/fullerene-kernel/src/main.rs
#![no_std]
#![no_main]

use spin::once::Once;
use spin::Mutex;

mod vga;

// Replace SERIAL static with VGA_BUFFER static
static VGA_BUFFER: Once<Mutex<vga::VgaBuffer>> = Once::new();

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    VGA_BUFFER.call_once(|| Mutex::new(vga::VgaBuffer::new()));
    let mut writer = VGA_BUFFER.get().unwrap().lock();
    writer.clear_screen(); // Clear screen on boot
    writer.write_string("Hello QEMU by fullerene!\n");
    writer.write_string("This is output directly to VGA.\n");
    loop {}
}
