//! Heap memory management module for Fullerene OS
//!
//! This module provides dynamic memory allocation, page table management,
//! frame allocation, and memory mapping utilities.

pub mod allocator;
pub mod memory_map;
pub mod paging;

pub use allocator::{ALLOCATOR, Heap, Locked, HEAP_SIZE};
pub use memory_map::{init_frame_allocator, MEMORY_MAP};
// Note: MAPPER and FRAME_ALLOCATOR are pub(crate), not re-exportable
pub use paging::{
    init, init_page_table, reinit_page_table, allocate_heap_from_map,
    HIGHER_HALF_OFFSET, PHYSICAL_MEMORY_OFFSET
};

// Re-export for convenience
pub use petroleum::page_table::BootInfoFrameAllocator;
pub use petroleum::page_table::EfiMemoryDescriptor;
pub use x86_64::structures::paging::{FrameAllocator, Mapper, OffsetPageTable, PageTableFlags as Flags, PhysFrame, Size4KiB};
pub use x86_64::{PhysAddr, VirtAddr};
