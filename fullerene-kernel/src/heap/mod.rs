//! Heap memory management module for Fullerene OS
//!
//! This module provides frame allocation and memory mapping utilities.
//! Dynamic allocation uses the global linked_list_allocator.

pub mod memory_map;

// Note: MAPPER and FRAME_ALLOCATOR are pub(crate), not re-exportable
pub use memory_map::{MEMORY_MAP, init_frame_allocator};

// Heap allocation functions moved to petroleum subcrate
pub use petroleum::page_table::BootInfoFrameAllocator;
pub use petroleum::page_table::EfiMemoryDescriptor;
pub use petroleum::page_table::allocate_heap_from_map;
pub use petroleum::page_table::reinit_page_table;
pub use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, PageTableFlags as Flags, PhysFrame, Size4KiB,
};
pub use x86_64::{PhysAddr, VirtAddr};

// Heap size constant moved to petroleum - for now define locally
pub const HEAP_SIZE: usize = 1024 * 1024; // 1MB heap
