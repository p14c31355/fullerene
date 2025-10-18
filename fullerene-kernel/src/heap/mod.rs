//! Heap memory management module for Fullerene OS
//!
//! This module provides frame allocation and memory mapping utilities.
//! Dynamic allocation uses the global linked_list_allocator.

pub mod memory_map;

// Note: MAPPER and FRAME_ALLOCATOR are pub(crate), not re-exportable
pub use memory_map::init_frame_allocator;

pub use petroleum::page_table::{get_higher_half_offset, reinit_page_table_with_allocator, BootInfoFrameAllocator};

// Heap size constant moved to petroleum - for now define locally
pub const HEAP_SIZE: usize = 1024 * 1024; // 1MB heap

/// Reinitialize the page table with identity mapping and higher-half kernel mapping
/// This is a wrapper around reinit_page_table_with_allocator for simple cases
pub fn reinit_page_table(
    kernel_phys_start: x86_64::PhysAddr,
    fb_addr: Option<x86_64::VirtAddr>,
    fb_size: Option<u64>,
) -> x86_64::VirtAddr {
    let mut frame_allocator = memory_map::FRAME_ALLOCATOR.get().expect("Frame allocator not initialized").lock();
    reinit_page_table_with_allocator(kernel_phys_start, fb_addr, fb_size, &mut *frame_allocator)
}
