#![no_std]
#![no_main]

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

fn vga_print(s: &str) {
    let vga_buffer = 0xb8000 as *mut u8;
    for (i, &b) in s.as_bytes().iter().enumerate() {
        unsafe {
            *vga_buffer.offset(i as isize * 2) = b;
            *vga_buffer.offset(i as isize * 2 + 1) = 0x0f;
        }
    }
}

#[repr(C)]
struct ElfHeader {
    magic: [u8; 4],
    _rest: [u8; 12],
    entry: u64,
}

fn load_kernel(kernel: &[u8]) -> Option<extern "C" fn() -> !> {
    if &kernel[0..4] != b"\x7FELF" {
        vga_print("Not an ELF file!");
        return None;
    }

    let header = unsafe { &*(kernel.as_ptr() as *const ElfHeader) };
    let entry: extern "C" fn() -> ! = unsafe { core::mem::transmute(header.entry) };
    Some(entry)
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    vga_print("bellows: bootloader started");

    let kernel_image: &[u8] = include_bytes!("../../target/x86_64-uefi/debug/fullerene-kernel");

    if let Some(kernel_entry) = load_kernel(kernel_image) {
        vga_print("Jumping to kernel...");
        kernel_entry();
    } else {
        vga_print("Failed to load kernel");
    }

    loop {}
}
