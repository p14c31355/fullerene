//! Core system types
//!
//! This module defines the core types used throughout the Fullerene kernel system.

use core::*;

// Note: SystemError and SystemResult are re-exported at the crate root in lib.rs

// Define PageFlags if not already defined
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFlags(u64);

impl PageFlags {
    pub fn kernel_data() -> Self {
        use x86_64::structures::paging::PageTableFlags as Flags;
        PageFlags((Flags::PRESENT | Flags::WRITABLE).bits())
    }

    pub fn user_data() -> Self {
        use x86_64::structures::paging::PageTableFlags as Flags;
        PageFlags((Flags::PRESENT | Flags::WRITABLE | Flags::USER_ACCESSIBLE).bits())
    }

    pub fn executable() -> Self {
        use x86_64::structures::paging::PageTableFlags as Flags;
        PageFlags((Flags::PRESENT | Flags::WRITABLE | Flags::USER_ACCESSIBLE | Flags::NO_EXECUTE).bits() ^ Flags::NO_EXECUTE.bits())
    }

    pub fn user_executable() -> Self {
        use x86_64::structures::paging::PageTableFlags as Flags;
        PageFlags((Flags::PRESENT | Flags::WRITABLE | Flags::USER_ACCESSIBLE).bits())
    }

    pub fn new(flags: u64) -> Self {
        PageFlags(flags)
    }

    pub fn flags(&self) -> u64 {
        self.0
    }

    pub fn as_u64(&self) -> u64 {
        self.0
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
