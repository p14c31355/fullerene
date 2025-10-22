// Fullerene OS Kernel
// Spacer in main.rs due to Rust unstable features
#![feature(abi_x86_interrupt)]
#![feature(non_exhaustive_omitted_patterns_lint)]
// fullerene-kernel/src/main.rs
#![no_std]
#![no_main]

// Kernel modules
mod boot;
mod context_switch; // Context switching
mod errors; // System error types
mod fs; // Basic filesystem
mod gdt; // Add GDT module
mod graphics;
mod heap;
mod init;
mod interrupts;
mod keyboard; // Keyboard input driver
mod loader; // Program loader
mod memory;
mod memory_management; // Virtual memory management
mod process; // Process management
mod scheduler;
mod shell;
mod syscall;
mod vga;

#[macro_use]
extern crate petroleum;

extern crate alloc;

use spin::Once;

// Global allocator removed - handled by petroleum crate

use petroleum::page_table::EfiMemoryDescriptor;

static MEMORY_MAP: Once<&'static [EfiMemoryDescriptor]> = Once::new();

const VGA_BUFFER_ADDRESS: usize = 0xb8000;
const VGA_COLOR_GREEN_ON_BLACK: u16 = 0x0200;

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Log panic info to serial port for debugging
    use petroleum::halt_loop;
    use petroleum::serial::_print;

    _print(format_args!("KERNEL PANIC: {}\n", info));

    // Visual indicator on VGA screen for kernel panic (yellow on red) - helps with debugging in environments without serial access
    let panic_msg = b"PANIC!";
    for (i, &ch) in panic_msg.iter().enumerate() {
        petroleum::volatile_write!(
            (VGA_BUFFER_ADDRESS + i * 2) as *mut u16,
            0xCE00 | (ch as u16)
        );
    }

    // Halt the CPU
    halt_loop();
}
