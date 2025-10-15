//! Heap memory management module for Fullerene OS
//!
//! This module provides frame allocation and memory mapping utilities.
//! Dynamic allocation uses the global linked_list_allocator.

pub mod memory_map;

// Note: MAPPER and FRAME_ALLOCATOR are pub(crate), not re-exportable
pub use memory_map::init_frame_allocator;

pub use petroleum::page_table::reinit_page_table;

// Heap size constant moved to petroleum - for now define locally
pub const HEAP_SIZE: usize = 1024 * 1024; // 1MB heap
