//! Heap memory management module for Fullerene OS
//!
//! This module provides frame allocation and memory mapping utilities.
//! Dynamic allocation uses the global linked_list_allocator.

pub use petroleum::page_table::BootInfoFrameAllocator;

pub const HEAP_SIZE: usize = 1024 * 1024; // 1MB heap
pub const KERNEL_STACK_SIZE: usize = 4096 * 64; // 256KB

use petroleum::page_table::MemoryDescriptorValidator;
use petroleum::page_table::memory_map::{EfiMemoryDescriptor, MemoryMapDescriptor};
use spin::Mutex;

/// Global frame allocator
pub(crate) static FRAME_ALLOCATOR: Mutex<Option<BootInfoFrameAllocator>> = Mutex::new(None);

/// Global memory map storage
pub static MEMORY_MAP: Mutex<Option<&'static [MemoryMapDescriptor]>> = Mutex::new(None);

/// Buffer for memory map descriptors to avoid heap allocation during init
pub const MAX_DESCRIPTORS: usize = 2048;

/// Static buffer for the global allocator to avoid early boot page faults
#[repr(align(4096))]
pub struct HeapBuffer(pub(crate) [u8; HEAP_SIZE]);

#[unsafe(link_section = ".data")]
pub static mut BOOT_HEAP_BUFFER: HeapBuffer = HeapBuffer([0; HEAP_SIZE]);

#[unsafe(link_section = ".data")]
pub(crate) static mut MEMORY_MAP_BUFFER: [MemoryMapDescriptor; MAX_DESCRIPTORS] = [const {
    MemoryMapDescriptor {
        ptr: core::ptr::null(),
        descriptor_size: 0,
    }
}; MAX_DESCRIPTORS];

/// Initialize the boot frame allocator with memory map
pub fn init_frame_allocator(memory_map: &[impl MemoryDescriptorValidator]) {
    // SAFETY: We are converting a slice of trait objects to a concrete slice of MemoryMapDescriptor.
    // The memory_map is guaranteed to contain valid MemoryMapDescriptor instances, so this is safe.
    let concrete_map = unsafe {
        core::slice::from_raw_parts(
            memory_map.as_ptr() as *const petroleum::page_table::memory_map::MemoryMapDescriptor,
            memory_map.len(),
        )
    };

    let allocator = petroleum::page_table::BitmapFrameAllocator::init_with_memory_map(concrete_map);
    *FRAME_ALLOCATOR.lock() = Some(allocator);
}
