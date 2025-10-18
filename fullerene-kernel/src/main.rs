// Fullerene OS Kernel
// Spacer in main.rs due to Rust unstable features
#![feature(abi_x86_interrupt)]
#![feature(non_exhaustive_omitted_patterns_lint)]
// fullerene-kernel/src/main.rs
#![no_std]
#![no_main]

// Kernel modules
mod context_switch; // Context switching
mod fs; // Basic filesystem
mod gdt; // Add GDT module
mod graphics;
mod heap;
mod interrupts;
mod keyboard; // Keyboard input driver
mod loader; // Program loader
mod memory_management; // Virtual memory management
mod process; // Process management
mod scheduler;
mod shell;
mod syscall;
mod traits;
mod vga; // System calls // Shell/CLI interface

// Submodules for modularizing main.rs
mod boot;
mod init;
mod memory;

extern crate alloc;
extern crate fullerene_kernel;

use spin::Once;

// Global allocator removed - handled by petroleum crate

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use petroleum::serial::_print;
    use x86_64::instructions::hlt;

    _print(format_args!("KERNEL PANIC: {}\n", info));

    // Visual indicator on VGA screen for kernel panic
    // Yellow text on red background for panic
    petroleum::volatile_write!((VGA_BUFFER_ADDRESS + 0) as *mut u16, 0xCE50); // 'P' yellow on red
    petroleum::volatile_write!((VGA_BUFFER_ADDRESS + 2) as *mut u16, 0xCE41); // 'A' yellow on red
    petroleum::volatile_write!((VGA_BUFFER_ADDRESS + 4) as *mut u16, 0xCE4E); // 'N' yellow on red
    petroleum::volatile_write!((VGA_BUFFER_ADDRESS + 6) as *mut u16, 0xCE49); // 'I' yellow on red
    petroleum::volatile_write!((VGA_BUFFER_ADDRESS + 8) as *mut u16, 0xCE43); // 'C' yellow on red
    petroleum::volatile_write!((VGA_BUFFER_ADDRESS + 10) as *mut u16, 0xCE21); // '!' yellow on red
    

    loop {
        hlt(); // Use hlt to halt the CPU in case of a kernel panic
    }
}

use petroleum::page_table::EfiMemoryDescriptor;

static MEMORY_MAP: Once<&'static [EfiMemoryDescriptor]> = Once::new();

const VGA_BUFFER_ADDRESS: usize = 0xb8000;
const VGA_COLOR_GREEN_ON_BLACK: u16 = 0x0200;
