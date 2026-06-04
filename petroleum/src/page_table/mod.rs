//! Page table management module.

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

pub use allocator::bitmap::BitmapFrameAllocator;
pub use allocator::traits::FrameAllocatorExt;
pub use constants::{BootInfoFrameAllocator, HIGHER_HALF_OFFSET as KERNEL_OFFSET};
pub use heap::{ALLOCATOR, HEAP_INITIALIZED};
pub use kernel::init::init;
pub use kernel::init::{InitAndJumpArgs, active_level_4_table, init_and_jump};
pub use kernel::mapper::Mapper as KernelMapper;
pub use memory_map::MemoryMapDescriptor;
pub use process::table::ProcessPageTable;
pub use raw::huge::map_range_with_huge_pages;
pub use raw::utils::{
    map_identity_range, map_range_4kiB, map_range_with_log_macro, map_to_higher_half_with_log,
    map_to_higher_half_with_log_macro, unmap_page_range,
};
pub use types::PageTableEntry as Pte;
pub use types::*;
pub type EfiMemoryDescriptor = memory_map::MemoryMapDescriptor;

pub fn init_kernel_mapper() {}
pub fn find_free_virtual_address(_size: u64) -> Option<u64> {
    None
}
pub fn dump_page_table_walk(_root: &types::PageTable, _virt: types::CanonicalVirtAddr) {}
