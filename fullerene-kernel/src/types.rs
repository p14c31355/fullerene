//! Core system types
//!
//! This module defines the core types used throughout the Fullerene kernel system.

use core::*;

// Use x86_64's PageTableFlags directly to reduce code duplication
pub use x86_64::structures::paging::PageTableFlags as PageFlags;

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
