//! Core system types
//!
//! This module defines the core types used throughout the Fullerene kernel system.

use crate::*;

// Re-export core types
pub use crate::{
    SystemError,
    SystemResult,
    LogLevel,
    PageFlags,
};

// Re-export memory management types
pub use crate::memory_management::{
    AllocError,
    BitmapFrameAllocator,
    FreeError,
    MapError,
    PageTableManager,
    ProcessMemoryManagerImpl,
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
