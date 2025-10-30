//! Heap memory management module for Fullerene OS
//!
//! This module provides frame allocation and memory mapping utilities.
//! Dynamic allocation uses the global linked_list_allocator.

// Note: MAPPER and FRAME_ALLOCATOR are pub(crate), not re-exportable

pub use petroleum::page_table::{BootInfoFrameAllocator, reinit_page_table_with_allocator};

// Heap size constant moved to petroleum - for now define locally
pub const HEAP_SIZE: usize = 1024 * 1024; // 1MB heap

// Kernel stack size for UEFI boot initialization
pub const KERNEL_STACK_SIZE: usize = 4096 * 16; // 64KB

/// Reinitialize the page table with identity mapping and higher-half kernel mapping
/// This is a wrapper around reinit_page_table_with_allocator for simple cases
pub fn reinit_page_table(
    kernel_phys_start: x86_64::PhysAddr,
    fb_addr: Option<x86_64::VirtAddr>,
    fb_size: Option<u64>,
) -> x86_64::VirtAddr {
    let mut frame_allocator = FRAME_ALLOCATOR
        .get()
        .expect("Frame allocator not initialized")
        .lock();
    let memory_map = MEMORY_MAP.get().expect("Memory map not initialized");
    reinit_page_table_with_allocator(
        kernel_phys_start,
        fb_addr,
        fb_size,
        &mut *frame_allocator,
        memory_map,
        x86_64::VirtAddr::new(0),
    )
}

use petroleum::common::EfiMemoryType;
use petroleum::page_table::efi_memory::{EfiMemoryDescriptor, MemoryMapDescriptor, MemoryDescriptorValidator};
use spin::{Mutex, Once};

/// Global frame allocator
pub(crate) static FRAME_ALLOCATOR: Once<Mutex<BootInfoFrameAllocator>> = Once::new();

/// Global memory map storage
pub static MEMORY_MAP: Once<&[MemoryMapDescriptor]> = Once::new();

/// Initialize the boot frame allocator with memory map
pub fn init_frame_allocator<T: MemoryDescriptorValidator>(memory_map: &[T]) {
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
