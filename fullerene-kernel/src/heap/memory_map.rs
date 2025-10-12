//! Memory map handling for heap initialization
//!
//! This module manages UEFI memory maps and frame allocation.

use petroleum::page_table::{BootInfoFrameAllocator, EfiMemoryDescriptor};
use petroleum::common::EfiMemoryType;
use spin::{Mutex, Once};

/// Global memory map storage
pub static MEMORY_MAP: Once<&'static [EfiMemoryDescriptor]> = Once::new();

/// Initialize the boot frame allocator with memory map
pub fn init_frame_allocator(memory_map: &'static [EfiMemoryDescriptor]) {
    let allocator = unsafe { BootInfoFrameAllocator::init(memory_map) };
    super::paging::FRAME_ALLOCATOR.call_once(|| Mutex::new(allocator));
    MEMORY_MAP.call_once(|| memory_map);
}

/// Helper function to iterate over memory descriptors with specific types
pub fn for_each_memory_descriptor<F>(
    memory_map: &[EfiMemoryDescriptor],
    types: &[EfiMemoryType],
    mut f: F,
) where
    F: FnMut(&EfiMemoryDescriptor),
{
    for desc in memory_map {
        if types.iter().any(|&t| desc.type_ == t) && desc.number_of_pages > 0 {
            f(desc);
        }
    }
}

// Global frame allocator is defined in paging module
