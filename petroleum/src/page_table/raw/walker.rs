//! Unified page table walker with automatic table creation.
//!
//! This module provides a single `walk_or_create` function that handles
//! all 4 levels of page table traversal, eliminating duplicated walk logic
//! across map/unmap/translate operations.

use crate::page_table::types::*;
use crate::page_table::PageTableEntry;

/// Errors that can occur during page table walking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkError {
    /// An intermediate entry points to a huge page where a table was expected.
    HugePageConflict {
        /// The level at which the conflict was found (2 = PD, 3 = PDPT).
        level: u8,
    },
    /// The frame allocator ran out of memory.
    OutOfMemory,
    /// The entry is present but points to an invalid physical address.
    InvalidEntry {
        level: u8,
    },
}

impl fmt::Display for WalkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WalkError::HugePageConflict { level } => {
                write!(f, "huge page conflict at level {}", level)
            }
            WalkError::OutOfMemory => write!(f, "out of memory during page table walk"),
            WalkError::InvalidEntry { level } => {
                write!(f, "invalid entry at level {}", level)
            }
        }
    }
}

use core::fmt;

/// Trait for frame allocators used during page table walking.
///
/// This is a simplified version of the full FrameAllocator trait,
/// focused on the needs of the walker.
pub trait FrameAlloc {
    /// Allocate a single 4 KiB frame, zeroed.
    ///
    /// Returns the physical address of the frame, or `None` on OOM.
    fn alloc_zeroed(&mut self) -> Option<u64>;
}

/// Walk the page table from the given root, creating intermediate tables as needed.
///
/// This is the **single unified function** for all page table traversal with creation.
/// It handles levels 4→1 in a loop, eliminating duplicated walk logic.
///
/// # Arguments
/// * `root` — The root (PML4) page table.
/// * `virt` — The virtual address to walk to.
/// * `allocator` — Frame allocator for creating new page tables.
/// * `target_level` — The level to walk to (1 = PT, 2 = PD, 3 = PDPT).
///
/// # Returns
/// A mutable reference to the page table entry at the target level.
///
/// # Safety
/// The caller must ensure that `root` is a valid, mapped page table and that
/// the physical addresses in entries point to valid, mapped page tables.
pub fn walk_or_create<'a, A: FrameAlloc>(
    root: &'a mut PageTable,
    virt: CanonicalVirtAddr,
    allocator: &mut A,
    target_level: u8,
) -> Result<&'a mut PageTableEntry, WalkError> {
    assert!(
        (1..=3).contains(&target_level),
        "target_level must be 1, 2, or 3"
    );

    let mut table = root;

    for level in (target_level + 1..=4).rev() {
        let idx = virt.index(level);
        let entry = &mut table[idx];

        if entry.is_unused() {
            // Need to allocate a new table
            let frame_addr = allocator.alloc_zeroed().ok_or(WalkError::OutOfMemory)?;
            *entry = PageTableEntry::new_with_frame(
                PhysFrame { start_address: frame_addr },
                Flags::PRESENT | Flags::WRITABLE | Flags::USER_ACCESSIBLE,
            );
        } else if entry.is_huge() {
            // Huge page in the middle of the walk — conflict
            return Err(WalkError::HugePageConflict { level });
        }

        // Safety: The entry is present and not huge, so it points to a valid
        // page table. The caller guarantees that all intermediate tables are mapped.
        table = unsafe { &mut *(entry.addr() as *mut PageTable) };
    }

    // Now return the entry at the target level
    let idx = virt.index(target_level);
    Ok(&mut table[idx])
}

/// Walk the page table **without** creating new tables.
///
/// Returns `Err(WalkError::OutOfMemory)` if an entry is unused (since we can't create).
/// This is used for translate/unmap operations where creation is not desired.
///
/// # Safety
/// Same as `walk_or_create`.
pub fn walk<'a>(
    root: &'a mut PageTable,
    virt: CanonicalVirtAddr,
    target_level: u8,
) -> Result<&'a mut PageTableEntry, WalkError> {
    assert!(
        (1..=3).contains(&target_level),
        "target_level must be 1, 2, or 3"
    );

    let mut table = root;

    for level in (target_level + 1..=4).rev() {
        let idx = virt.index(level);
        let entry = &mut table[idx];

        if entry.is_unused() {
            return Err(WalkError::OutOfMemory); // Entry doesn't exist
        } else if entry.is_huge() {
            return Err(WalkError::HugePageConflict { level });
        }

        table = unsafe { &mut *(entry.addr() as *mut PageTable) };
    }

    let idx = virt.index(target_level);
    Ok(&mut table[idx])
}

/// Walk to a specific level and return the **table** at that level.
///
/// # Safety
/// Same as `walk_or_create`.
pub fn walk_to_table<'a>(
    root: &'a mut PageTable,
    virt: CanonicalVirtAddr,
    target_level: u8,
) -> Result<&'a mut PageTable, WalkError> {
    assert!(
        (1..=3).contains(&target_level),
        "target_level must be 1, 2, or 3"
    );

    let mut table = root;

    for level in (target_level + 1..=4).rev() {
        let idx = virt.index(level);
        let entry = &table[idx];

        if entry.is_unused() {
            return Err(WalkError::OutOfMemory);
        } else if entry.is_huge() {
            return Err(WalkError::HugePageConflict { level });
        }

        table = unsafe { &mut *(entry.addr() as *mut PageTable) };
    }

    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A simple bump allocator for testing.
    struct TestAllocator {
        next_frame: u64,
    }

    impl TestAllocator {
        fn new(start: u64) -> Self {
            Self { next_frame: start }
        }
    }

    impl FrameAlloc for TestAllocator {
        fn alloc_zeroed(&mut self) -> Option<u64> {
            let addr = self.next_frame;
            self.next_frame += 4096;
            Some(addr)
        }
    }

    #[test]
    fn walk_creates_tables() {
        let mut root = PageTable::new();
        let mut alloc = TestAllocator::new(0x1000);
        let virt = CanonicalVirtAddr::new(0x0000_0000_0020_0000).unwrap(); // 2 MiB offset

        let entry = walk_or_create(&mut root, virt, &mut alloc, 1).unwrap();
        assert!(entry.is_present());
    }

    #[test]
    fn walk_fails_on_missing() {
        let mut root = PageTable::new();
        let virt = CanonicalVirtAddr::new(0x0000_0000_0020_0000).unwrap();

        let result = walk(&mut root, virt, 1);
        assert!(result.is_err());
    }
}