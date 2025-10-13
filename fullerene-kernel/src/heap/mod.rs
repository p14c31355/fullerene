//! Heap memory management module for Fullerene OS
//!
//! This module provides dynamic memory allocation, page table management,
//! frame allocation, and memory mapping utilities.

pub mod allocator;
pub mod memory_map;
pub mod paging;

pub use allocator::{ALLOCATOR, HEAP_SIZE, Heap, Locked};
pub use memory_map::{MEMORY_MAP, init_frame_allocator};
// Note: MAPPER and FRAME_ALLOCATOR are pub(crate), not re-exportable
pub use paging::{
    HIGHER_HALF_OFFSET, PHYSICAL_MEMORY_OFFSET, allocate_heap_from_map, init, init_page_table,
    reinit_page_table,
};

// Re-export for convenience
pub use petroleum::page_table::BootInfoFrameAllocator;
pub use petroleum::page_table::EfiMemoryDescriptor;
pub use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, PageTableFlags as Flags, PhysFrame, Size4KiB,
};
pub use x86_64::{PhysAddr, VirtAddr};
