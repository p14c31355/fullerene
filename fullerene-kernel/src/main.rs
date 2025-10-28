#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use petroleum::{scheduler_log, debug_log, periodic_task, mem_debug, draw_filled_rect, draw_border_rect, info_log, warn_log};

extern crate alloc;

// Constants
pub const VGA_BUFFER_ADDRESS: usize = 0xb8000;

// Exported globals
pub use heap::MEMORY_MAP;

// Module declarations
pub mod boot;
pub mod context_switch;
pub mod fs;
pub mod gdt;
pub mod graphics;
pub mod hardware;
pub mod heap;
pub mod init;
pub mod interrupts;
pub mod keyboard;
pub mod loader;
pub mod memory;
pub mod memory_management;
pub mod process;
pub mod scheduler;
pub mod shell;
pub mod syscall;
pub mod vga;

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write;
    use petroleum::println;
    println!("Kernel Panic: {}", info);
    loop {}
}
