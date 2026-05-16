#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]

use petroleum::{
    debug_log, draw_border_rect, draw_filled_rect, info_log, mem_debug, periodic_task,
    scheduler_log, warn_log,
};

extern crate alloc;

// Define panic and alloc error handlers using petroleum's macros
petroleum::define_panic_handler!();
petroleum::define_alloc_error_handler!();

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
