//! Core system types
//!
//! This module defines the core types used throughout the Fullerene kernel system.

use crate::*;

// Re-export core types from errors module
pub use crate::errors::SystemError;
pub type SystemResult<T> = Result<T, SystemError>;

// Define LogLevel if not already defined
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
}

// Define PageFlags if not already defined
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFlags(u64);

impl PageFlags {
    pub fn kernel_data() -> Self {
        PageFlags(0) // Placeholder implementation
    }
}

// Re-export traits
pub use crate::traits::*;

// Re-export memory management types - only public ones
pub use crate::memory_management::{
    AllocError,
    FreeError,
    MapError,
    ProcessPageTable,
    UnifiedMemoryManager,
    convenience,
};

// Re-export process types
pub use crate::process::{
    Process,
    ProcessState,
    ProcessContext,
    ProcessId,
};

// Re-export hardware types
pub use crate::graphics::vga_device::VgaDevice;
pub use crate::hardware::{
    DeviceManager,
    PciConfigSpace,
    PciDevice,
    PciScanner,
    HardwarePorts,
};

// Re-export critical types and functions for internal use
pub use crate::memory_management::{
    get_memory_manager,
    init_memory_manager,
};
pub use crate::process::PROCESS_LIST;

// Additional type definitions and utilities can be added here if needed
