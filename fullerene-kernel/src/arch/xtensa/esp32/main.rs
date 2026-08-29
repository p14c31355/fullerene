//! The Fullerene ESP32 kernel image entry.

#![feature(alloc_error_handler, asm_experimental_arch)]
#![no_std]
#![no_main]

#[unsafe(no_mangle)]
static mut ESP32_BOOT_STACK: [u8; 16 * 1024] = [0; 16 * 1024];

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    unsafe {
        core::arch::asm!(
            "l32r a1, 1f",
            "j 2f",
            ".align 4",
            "1: .word {stack} + {stack_size}",
            "2:",
            stack = sym ESP32_BOOT_STACK,
            stack_size = const 16 * 1024,
            options(nomem, nostack)
        );
        // The Rust target owns startup; ROM loads zero-filled BSS from the
        // ELF segment memory size. No Fullerene linker script is used.
    }
    fullerene_kernel::arch::xtensa::esp32::rust_entry()
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    fullerene_kernel::arch::xtensa::esp32::runtime::panic_report(info)
}

#[alloc_error_handler]
fn alloc_error(layout: core::alloc::Layout) -> ! {
    fullerene_kernel::arch::xtensa::esp32::runtime::alloc_error_report(layout)
}
