// Fullerene OS Kernel
// Spacer in main.rs due to Rust unstable features
#![feature(abi_x86_interrupt)]
#![feature(non_exhaustive_omitted_patterns_lint)]
// fullerene-kernel/src/main.rs
#![no_std]
#![no_main]

// Kernel modules
mod gdt; // Add GDT module
mod graphics;
mod heap;
mod interrupts;
mod vga;
mod context_switch; // Context switching
mod fs; // Basic filesystem
mod keyboard; // Keyboard input driver
mod loader; // Program loader
mod memory_management; // Virtual memory management
mod process; // Process management
mod shell;
mod syscall; // System calls // Shell/CLI interface

// Submodules for modularizing main.rs
mod boot;
mod init;
mod memory;
mod test_process;

extern crate alloc;

use spin::Once;

#[cfg(all(not(test), not(target_os = "uefi")))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    petroleum::handle_panic(info)
}

#[cfg(all(not(test), target_os = "uefi"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // For UEFI, just loop forever on panic
    loop {
        unsafe {
            x86_64::instructions::hlt();
        }
    }
}

use petroleum::page_table::EfiMemoryDescriptor;

static MEMORY_MAP: Once<&'static [EfiMemoryDescriptor]> = Once::new();

const VGA_BUFFER_ADDRESS: usize = 0xb8000;
const VGA_COLOR_GREEN_ON_BLACK: u16 = 0x0200;

// A simple loop that halts the CPU until the next interrupt
pub fn hlt_loop() -> ! {
    use x86_64::instructions::hlt;
    loop {
        hlt();
    }
}
