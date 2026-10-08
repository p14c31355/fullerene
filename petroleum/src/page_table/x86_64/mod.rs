//! x86 paging, the UEFI memory map, and the kernel heap.
//!
//! Only compiled on x86 targets; globbed into `page_table` so historic paths
//! (`petroleum::page_table::heap::...`, `crate::page_table::types::...`)
//! keep working.

pub mod allocator;
pub mod heap;
pub mod kernel;
pub mod memory_map;
pub mod pat;
pub mod pe;
pub mod process;
pub mod raw;
pub mod types;
pub mod virtual_memory;

pub use crate::page_table::constants::{
    BootInfoFrameAllocator, HIGHER_HALF_OFFSET as KERNEL_OFFSET,
};
pub use allocator::bitmap::BitmapFrameAllocator;
pub use allocator::traits::FrameAllocatorExt;
#[cfg(not(feature = "std"))]
pub use heap::ALLOCATOR;
pub use heap::{HEAP_INITIALIZED, init_global_heap};
pub use kernel::init::init;
pub use kernel::init::{
    InitAndJumpArgs, KernelMemoryOperations, active_level_4_table, init_and_jump,
};
pub use kernel::mapper::Mapper as KernelMapper;
pub use memory_map::MemoryMapDescriptor;
pub use process::table::ProcessPageTable;
pub use raw::huge::map_range_with_huge_pages;
pub use raw::utils::{map_identity_range, map_range_4kiB, map_to_higher_half_with_log};
pub use types::PageTableEntry as Pte;
pub use types::*;
pub type EfiMemoryDescriptor = memory_map::MemoryMapDescriptor;

/// Placeholder for the diagnostics dump helper.
pub fn dump_page_table_walk(_root: &types::PageTable, _virt: types::CanonicalVirtAddr) {}
