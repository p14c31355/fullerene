//! Core system traits
//!
//! This module defines the core traits used throughout the Fullerene kernel system.

use crate::*;

// Re-export core traits
pub use crate::{
    Initializable,
    ErrorLogging,
    HardwareDevice,
    MemoryManager,
    ProcessMemoryManager,
    PageTableHelper,
    FrameAllocator,
    SyscallHandler,
    Logger,
};

// Additional trait implementations can be added here if needed
