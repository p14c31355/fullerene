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

use core::panic::PanicInfo;

// Re-export consolidated logging types from petroleum - must come before traits mod to be available in traits.rs
pub use petroleum::common::logging::{SystemError, SystemResult};

// Re-export x86_64 page table flags as PageFlags for kernel-wide use
pub use x86_64::structures::paging::PageTableFlags as PageFlags;

// Remove ambiguous logging function imports to use macro-based logging exclusively

// Let petroleum provide its logging macros
#[macro_use]
extern crate petroleum;

extern crate alloc;

use petroleum::page_table::EfiMemoryDescriptor;
use spin::Once;

// Global system tick accessor (needed by shell)
pub fn get_system_tick() -> u64 {
    scheduler::get_system_tick()
}

// Submodules
pub mod errors;
pub mod initializer;
pub mod operations;
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

pub mod memory_management; // Virtual memory management
pub mod process; // Process management
pub mod shell;
pub mod syscall; // System calls // Shell/CLI interface
pub mod test_process;

// Submodules for modularizing main.rs
pub mod boot;
pub mod init;
pub mod memory;
pub mod scheduler;

// Re-export key types and functions from submodules for convenience
pub use initializer::{initialize_system, register_system_component};

// Re-export traits with explicit imports to avoid conflicts

pub use traits::HardwareDevice;
pub use traits::{
    ErrorLogging, FrameAllocator, Initializable, MemoryManager,
    ProcessMemoryManager, SyscallHandler,
};
pub use petroleum::page_table::PageTableHelper;

// Re-export memory management types
pub use memory_management::{FreeError, ProcessPageTable, UnifiedMemoryManager};

// Re-export commonly used types for convenience
pub use graphics::vga_device::VgaDevice;
pub use hardware::{
    HardwarePorts, PciConfigSpace, PciDevice, PciScanner, device_manager::DeviceManager,
};
pub use memory_management::{AllocError, MapError};
// Re-export critical types from memory_management module for internal use
pub use memory_management::{get_memory_manager, init_memory_manager};
pub use process::{PROCESS_LIST, Process, ProcessId};

pub static MEMORY_MAP: Once<&'static [EfiMemoryDescriptor]> = Once::new();


const VGA_BUFFER_ADDRESS: usize = 0xb8000;
const VGA_COLOR_GREEN_ON_BLACK: u16 = 0x0200;

// A graphics testing loop integrated with full system scheduling
pub fn graphics_test_loop() -> ! {
    use x86_64::instructions::hlt;

    // Test our SimpleFramebuffer (Redox vesad-style)
    if let Some(mut fb) = crate::graphics::framebuffer::get_simple_framebuffer() {
        crate::graphics::_print(format_args!("Graphics: Testing SimpleFramebuffer API\n"));
        fb.clear(0xFF000000); // Clear to black

        // Test draw_pixel (orbclient-style)
        for i in 0..100 {
            fb.draw_pixel(i, i, 0xFFFF0000); // Red diagonal line
            fb.draw_pixel(200 + i, 100, 0xFF00FF00); // Green horizontal line
            fb.draw_pixel(100, 200 + i, 0xFF0000FF); // Blue vertical line
        }

        // Test draw_rect (orbclient-style)
        fb.draw_rect(50, 50, 100, 50, 0xFFFFFF00); // Yellow rectangle
        fb.draw_rect(300, 300, 80, 60, 0xFFFF00FF); // Magenta rectangle
        fb.draw_rect(150, 400, 60, 40, 0xFF00FFFF); // Cyan rectangle

        crate::graphics::_print(format_args!("Graphics: SimpleFramebuffer drawing completed\n"));

        log::info!("Graphics: Starting full system scheduler after graphics test");

        // Now start the full scheduler to integrate all system functionality
        crate::scheduler::scheduler_loop();
    } else {
        crate::graphics::_print(format_args!("Graphics: ERROR - SimpleFramebuffer not initialized, falling back to scheduler anyway\n"));
        // Fallback to scheduler even without graphics
        crate::scheduler::scheduler_loop();
    }
}
