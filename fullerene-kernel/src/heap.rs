//! Heap memory management module for Fullerene OS
//!
//! This module provides frame allocation and memory mapping utilities.
//! Dynamic allocation uses the global linked_list_allocator.

// Note: MAPPER and FRAME_ALLOCATOR are pub(crate), not re-exportable

pub use petroleum::page_table::{BootInfoFrameAllocator, reinit_page_table_with_allocator};

// Heap size constant moved to petroleum - for now define locally
pub const HEAP_SIZE: usize = 1024 * 1024; // 1MB heap

// Kernel stack size for UEFI boot initialization
pub const KERNEL_STACK_SIZE: usize = 4096 * 64; // 256KB


use petroleum::common::EfiMemoryType;
use petroleum::page_table::efi_memory::{
    EfiMemoryDescriptor, MemoryDescriptorValidator, MemoryMapDescriptor,
};
use spin::{Mutex, Once};

/// Global frame allocator
pub(crate) static FRAME_ALLOCATOR: Once<Mutex<BootInfoFrameAllocator>> = Once::new();

/// Global memory map storage
pub static MEMORY_MAP: Once<&[MemoryMapDescriptor]> = Once::new();

/// Buffer for memory map descriptors to avoid heap allocation during init
pub const MAX_DESCRIPTORS: usize = 2048;
pub(crate) static mut MEMORY_MAP_BUFFER: [MemoryMapDescriptor; MAX_DESCRIPTORS] = [const {
    MemoryMapDescriptor {
        ptr: core::ptr::null(),
        descriptor_size: 0,
    }
};
    MAX_DESCRIPTORS];

/// Initialize the boot frame allocator with memory map
pub fn init_frame_allocator(memory_map: &[impl MemoryDescriptorValidator]) {
    let allocator = unsafe { BootInfoFrameAllocator::init(memory_map) };
    FRAME_ALLOCATOR.call_once(|| Mutex::new(allocator));
    // MEMORY_MAP is already initialized in setup_memory_maps
}

