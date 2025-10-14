//! Fullerene OS Kernel Library
//!
//! This library provides the core functionality for the Fullerene OS kernel,
//! including common traits, error types, and system abstractions.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]
#![feature(slice_ptr_get)]
#![feature(sync_unsafe_cell)]
#![feature(vec_into_raw_parts)]

extern crate alloc;

use spin::Once;
use petroleum::page_table::EfiMemoryDescriptor;

// Submodules
pub mod errors;
pub mod initializer;
pub mod logging;
pub mod operations;
pub mod types;

// Kernel modules
pub mod gdt; // Add GDT module
pub mod graphics;
pub mod hardware;
pub mod heap;
pub mod interrupts;
pub mod traits;
pub mod vga;

// Kernel modules
pub mod context_switch; // Context switching
pub mod fs; // Basic filesystem
pub mod keyboard; // Keyboard input driver
pub mod loader; // Program loader
// Logging macros with #[macro_export] are available at crate root
pub mod macros; // Logging and utility macros
pub mod memory_management; // Virtual memory management
pub mod process; // Process management
pub mod shell;
pub mod syscall; // System calls // Shell/CLI interface

// Submodules for modularizing main.rs
pub mod boot;
pub mod init;
pub mod memory;
pub mod test_process;

// Re-export key types and functions from submodules for convenience
pub use errors::{SystemError, SystemResult};
pub use initializer::{initialize_system, register_system_component};
pub use logging::{get_global_log_level, init_global_logger, log_debug, log_error, log_info, log_trace, log_warning};
pub use types::*;
pub use traits::*;

// Re-export memory management types
pub use memory_management::{FreeError, ProcessPageTable, UnifiedMemoryManager};

// Re-export commonly used types for convenience
pub use graphics::vga_device::VgaDevice;
pub use hardware::{
    device_manager::DeviceManager,
    PciConfigSpace,
    PciDevice,
    PciScanner,
    HardwarePorts,
};
pub use memory_management::{
    AllocError, MapError,
};
// Re-export critical types from memory_management module for internal use
pub use memory_management::{get_memory_manager, init_memory_manager};
pub use process::{PROCESS_LIST, Process, ProcessId};

static MEMORY_MAP: Once<&'static [EfiMemoryDescriptor]> = Once::new();

const VGA_BUFFER_ADDRESS: usize = 0xb8000;
const VGA_COLOR_GREEN_ON_BLACK: u16 = 0x0200;

// A simple loop that halts the CPU until the next interrupt
pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}
