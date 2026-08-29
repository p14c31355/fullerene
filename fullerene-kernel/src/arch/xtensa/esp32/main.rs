//! The Fullerene ESP32 kernel image entry.

#![feature(alloc_error_handler, asm_experimental_arch)]
#![no_std]
#![no_main]

unsafe extern "C" {
    static mut __bss_start: u8;
    static __bss_end: u8;
}

#[unsafe(no_mangle)]
static mut ESP32_BOOT_STACK: [u8; 16 * 1024] = [0; 16 * 1024];

core::arch::global_asm!(
    ".section .text._start, \"ax\"",
    ".p2align 4",
    ".global ESP32_BOOT_STACK_LITERAL",
    "ESP32_BOOT_STACK_LITERAL:",
    ".word {stack} + {stack_size}",
    ".p2align 4",
    ".global _start",
    "_start:",
    "l32r a1, ESP32_BOOT_STACK_LITERAL",
    "call0 {entry}",
    ".size _start, . - _start",
    stack = sym ESP32_BOOT_STACK,
    stack_size = const 16 * 1024,
    entry = sym esp32_start_rust,
);

#[unsafe(no_mangle)]
extern "C" fn esp32_start_rust() -> ! {
    esp32_zero_bss();
    fullerene_kernel::arch::xtensa::esp32::rust_entry()
}

fn esp32_zero_bss() {
    // The ESP ROM copies PT_LOAD data but does not initialize PT_LOAD BSS.
    // Keep the firmware image small and clear it explicitly before Rust code
    // can observe any zero-initialized static.
    unsafe {
        let mut pointer = core::ptr::addr_of_mut!(__bss_start);
        let end = core::ptr::addr_of!(__bss_end) as *mut u8;
        while pointer < end {
            pointer.write_volatile(0);
            pointer = pointer.add(1);
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    fullerene_kernel::arch::xtensa::esp32::runtime::panic_report(info)
}

#[alloc_error_handler]
fn alloc_error(layout: core::alloc::Layout) -> ! {
    fullerene_kernel::arch::xtensa::esp32::runtime::alloc_error_report(layout)
}
