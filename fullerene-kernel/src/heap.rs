//! Heap memory management module for Fullerene OS
//!
//! This module provides frame allocation and memory mapping utilities.
//! Dynamic allocation uses the global linked_list_allocator.

// Note: MAPPER and FRAME_ALLOCATOR are pub(crate), not re-exportable

pub use petroleum::page_table::BootInfoFrameAllocator;

// Heap size constant moved to petroleum - for now define locally
pub const HEAP_SIZE: usize = 1024 * 1024; // 1MB heap

// Kernel stack size for UEFI boot initialization
pub const KERNEL_STACK_SIZE: usize = 4096 * 64; // 256KB

use petroleum::common::EfiMemoryType;
use petroleum::page_table::MemoryDescriptorValidator;
use petroleum::page_table::memory_map::{EfiMemoryDescriptor, MemoryMapDescriptor};
use spin::{Mutex, Once};

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
};
    MAX_DESCRIPTORS];

/// Initialize the boot frame allocator with memory map
pub fn init_frame_allocator(memory_map: &[impl MemoryDescriptorValidator]) {
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: init_frame_allocator start\n");

    // Convert &[impl MemoryDescriptorValidator] to &[MemoryMapDescriptor]
    // Since MemoryMapDescriptor is the concrete type that implements the trait,
    // and we know that's what is being passed from uefi_init.
    let concrete_map = unsafe {
        core::slice::from_raw_parts(
            memory_map.as_ptr() as *const petroleum::page_table::memory_map::MemoryMapDescriptor,
            memory_map.len(),
        )
    };

    let allocator = petroleum::page_table::BitmapFrameAllocator::init_with_memory_map(concrete_map);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: BootInfoFrameAllocator::init done\n");
    let mut lock = FRAME_ALLOCATOR.lock();
    *lock = Some(allocator);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: init_frame_allocator complete\n");
}
