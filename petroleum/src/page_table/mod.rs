//! Page table management module.
//!
//! Provides types and functions for x86_64 page table manipulation,
//! including a unified walker, huge page support, and process page tables.

pub mod allocator;
pub mod constants;
pub mod heap;
pub mod kernel;
pub mod memory_map;
pub mod pe;
pub mod process;
pub mod raw;
pub mod types;

#[cfg(test)]
mod tests;

pub use types::*;

// ── Re-exports for backward compatibility ─────────────────────────────

/// Old name for `PageTableEntry`.
pub use types::PageTableEntry as Pte;

/// Re-export of `BitmapFrameAllocator`.
pub use allocator::bitmap::BitmapFrameAllocator;

/// Re-export of `FrameAllocatorExt` trait.
pub use allocator::traits::FrameAllocatorExt;

/// Re-export of kernel init types and functions.
pub use kernel::init::{InitAndJumpArgs, active_level_4_table, init_and_jump};

/// Re-export of `KernelMapper` (now `Mapper`).
pub use kernel::mapper::Mapper as KernelMapper;

/// Re-export of process page table.
pub use process::table::ProcessPageTable;

/// Re-export of memory map types.
pub use memory_map::MemoryMapDescriptor;

/// Re-export of constants.
pub use constants::{BootInfoFrameAllocator, HIGHER_HALF_OFFSET as KERNEL_OFFSET};

/// Re-export of heap globals.
pub use heap::{ALLOCATOR, HEAP_INITIALIZED};

/// Re-export of backward-compat function aliases.
pub use raw::utils::{
    get_memory_stats, map_identity_range_checked, map_page_range, map_range_with_huge_pages,
    map_range_with_log_macro, map_to_higher_half_with_log_macro, unmap_page_range,
};

/// Re-export of `init` function.
pub use kernel::init::init;

// ── Additional backward-compat stubs ──────────────────────────────────

/// Deprecated: Use `memory_map::MemoryMapDescriptor` instead.
pub type EfiMemoryDescriptor = memory_map::MemoryMapDescriptor;

/// Deprecated: No-op.
pub fn init_kernel_mapper() {}

/// Deprecated: Returns None.
pub fn find_free_virtual_address(_size: u64) -> Option<u64> {
    None
}

/// Deprecated: No-op.
pub fn dump_page_table_walk(_root: &types::PageTable, _virt: types::CanonicalVirtAddr) {}
