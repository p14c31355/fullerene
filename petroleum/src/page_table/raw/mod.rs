//! Raw page table operations: mapping, unmapping, translation.
//!
//! These are the low-level building blocks. Higher-level APIs should
//! use the kernel mapper or the declarative mapper instead.

pub mod huge;
pub mod mapper;
pub mod translate;
pub mod utils;
pub mod walker;

// Re-export commonly used items
pub use huge::{map_1g_checked, map_2m_checked, map_auto, HugeError};
pub use huge::map_range_with_huge_pages;
pub use mapper::{map_huge_1g, map_huge_2m, map_page, unmap_page, unmap_range};
pub use translate::{translate, translate_frame, translate_range, dump_page_table_walk};
pub use utils::{flush_tlb, flush_tlb_all, read_cr3, is_mapped, count_mapped, TEMP_VA_FOR_CLONE, TEMP_VA_FOR_DESTROY};
pub use walker::{walk, walk_or_create, walk_to_table, FrameAlloc, WalkError};
