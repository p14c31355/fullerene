//! Fullerene OS Kernel Library
//!
//! This library provides the core functionality for the Fullerene OS kernel,
//! including common traits, error types, and system abstractions.

#![no_std]
#![no_main]
#![macro_use]
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
// Logging macros with #[macro_export] are exported globally
#[macro_use]
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
pub use initializer::{initialize_system, register_system_component};
pub use types::PageFlags;

/// Helper function to log errors using petroleum's logging system
pub fn log_error_petroleum(error: &SystemError, context: &'static str) {
    // Convert our SystemError to petroleum's SystemError for logging
    let petroleum_error = match error {
        SystemError::InvalidSyscall => petroleum::common::logging::SystemError::InvalidSyscall,
        SystemError::BadFileDescriptor => petroleum::common::logging::SystemError::BadFileDescriptor,
        SystemError::PermissionDenied => petroleum::common::logging::SystemError::PermissionDenied,
        SystemError::FileNotFound => petroleum::common::logging::SystemError::FileNotFound,
        SystemError::NoSuchProcess => petroleum::common::logging::SystemError::NoSuchProcess,
        SystemError::InvalidArgument => petroleum::common::logging::SystemError::InvalidArgument,
        SystemError::SyscallOutOfMemory => petroleum::common::logging::SystemError::SyscallOutOfMemory,
        SystemError::FileExists => petroleum::common::logging::SystemError::FileExists,
        SystemError::BadFileDescriptor => petroleum::common::logging::SystemError::FsInvalidFileDescriptor,
        SystemError::InvalidSeek => petroleum::common::logging::SystemError::InvalidSeek,
        SystemError::DiskFull => petroleum::common::logging::SystemError::DiskFull,
        SystemError::MappingFailed => petroleum::common::logging::SystemError::MappingFailed,
        SystemError::UnmappingFailed => petroleum::common::logging::SystemError::UnmappingFailed,
        SystemError::FrameAllocationFailed => petroleum::common::logging::SystemError::FrameAllocationFailed,
        SystemError::MemOutOfMemory => petroleum::common::logging::SystemError::MemOutOfMemory,
        SystemError::InvalidFormat => petroleum::common::logging::SystemError::InvalidFormat,
        SystemError::LoadFailed => petroleum::common::logging::SystemError::LoadFailed,
        SystemError::DeviceNotFound => petroleum::common::logging::SystemError::DeviceNotFound,
        SystemError::DeviceError => petroleum::common::logging::SystemError::DeviceError,
        SystemError::PortError => petroleum::common::logging::SystemError::PortError,
        SystemError::NotImplemented => petroleum::common::logging::SystemError::NotImplemented,
        SystemError::NotSupported => petroleum::common::logging::SystemError::NotSupported,
        SystemError::InternalError => petroleum::common::logging::SystemError::InternalError,
        SystemError::UnknownError => petroleum::common::logging::SystemError::UnknownError,
    };
    petroleum::common::logging::log_error(&petroleum_error, context);
}

// Re-export consolidated logging system from petroleum
pub use petroleum::common::logging::{log_info, log_warning, log_debug, log_trace, SystemError as PetroleumSystemError};

// Re-export from errors module to avoid conflicts
pub use errors::{SystemError, SystemResult};

// Re-export traits with explicit imports to avoid conflicts
#[macro_use]
pub use traits::HardwareDevice;
pub use traits::{SyscallHandler, MemoryManager, ProcessMemoryManager,
                 PageTableHelper, FrameAllocator, Initializable, ErrorLogging};

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
