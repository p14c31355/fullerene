//! Memory map handling for heap initialization
//!
//! This module manages UEFI memory maps and frame allocation.

use petroleum::common::EfiMemoryType;
use petroleum::page_table::{BootInfoFrameAllocator, EfiMemoryDescriptor};
use spin::{Mutex, Once};

/// Global frame allocator
pub(crate) static FRAME_ALLOCATOR: Once<Mutex<BootInfoFrameAllocator<'static>>> = Once::new();

/// Global memory map storage
pub static MEMORY_MAP: Once<&'static [EfiMemoryDescriptor]> = Once::new();

/// Initialize the boot frame allocator with memory map
pub fn init_frame_allocator(memory_map: &'static [EfiMemoryDescriptor]) {
    let allocator = unsafe { BootInfoFrameAllocator::init(memory_map) };
    FRAME_ALLOCATOR.call_once(|| Mutex::new(allocator));
    // MEMORY_MAP is already initialized in setup_memory_maps
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
