#![no_std]
#![no_main]

// Entry point kernel
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Write VGA memory
    let vga_buffer = 0xb8000 as *mut u8;
    let message = b"Hello QEMU by fullerene!";
    for (i, &byte) in message.iter().enumerate() {
        unsafe {
            *vga_buffer.offset((i * 2) as isize) = byte;
            *vga_buffer.offset((i * 2 + 1) as isize) = 0x0f; // 白
        }
    }

    loop {}
}
