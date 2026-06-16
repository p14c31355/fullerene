//! Low-level page mapping operations.
//!
//! These functions use the unified walker internally.

use crate::page_table::PageTableEntry;
use crate::page_table::allocator::traits::FrameAllocator;
use crate::page_table::raw::walker::{FrameAlloc, WalkError, walk, walk_or_create};
use crate::page_table::types::*;

/// Adapter: FrameAllocator → walker::FrameAlloc
struct WalkerAdapter<'a, A: FrameAllocator>(&'a mut A);

impl<'a, A: FrameAllocator> FrameAlloc for WalkerAdapter<'a, A> {
    fn alloc_zeroed(&mut self) -> Option<u64> {
        self.0.allocate().ok().map(|f| f.start_address())
    }
}

/// Map a single 4 KiB page.
///
/// Creates intermediate page tables as needed.
pub fn map_page<A: FrameAllocator>(
    root: &mut PageTable,
    virt: CanonicalVirtAddr,
    frame: PhysFrame,
    flags: u64,
    allocator: &mut A,
) -> Result<(), WalkError> {
    let adapter = &mut WalkerAdapter(allocator);
    let entry = walk_or_create(root, virt, adapter, 1)?;
    *entry = PageTableEntry::new_with_frame(frame, flags);
    Ok(())
}

/// Map a 2 MiB huge page.
pub fn map_huge_2m<A: FrameAllocator>(
    root: &mut PageTable,
    virt: CanonicalVirtAddr,
    phys: u64,
    flags: u64,
    allocator: &mut A,
) -> Result<(), WalkError> {
    let adapter = &mut WalkerAdapter(allocator);
    let entry = walk_or_create(root, virt, adapter, 2)?;
    *entry = PageTableEntry::new(phys | flags | Flags::HUGE_PAGE);
    Ok(())
}

/// Map a 1 GiB huge page.
pub fn map_huge_1g<A: FrameAllocator>(
    root: &mut PageTable,
    virt: CanonicalVirtAddr,
    phys: u64,
    flags: u64,
    allocator: &mut A,
) -> Result<(), WalkError> {
    let adapter = &mut WalkerAdapter(allocator);
    let entry = walk_or_create(root, virt, adapter, 3)?;
    *entry = PageTableEntry::new(phys | flags | Flags::HUGE_PAGE);
    Ok(())
}

/// Unmap a single page.
///
/// Returns the physical frame that was mapped, if any.
pub fn unmap_page<A: FrameAllocator>(
    root: &mut PageTable,
    virt: CanonicalVirtAddr,
    allocator: &mut A,
) -> Result<Option<PhysFrame>, WalkError> {
    let entry = walk(root, virt, 1)?;

    if !entry.is_present() {
        return Ok(None);
    }

    let frame = PhysFrame::from_start_address(entry.addr())
        .expect("page table entry has unaligned address");
    entry.clear();
    allocator.deallocate(frame);

    Ok(Some(frame))
}

/// Unmap a range of 4 KiB pages.
pub fn unmap_range<A: FrameAllocator>(
    root: &mut PageTable,
    virt: CanonicalVirtAddr,
    size: u64,
    allocator: &mut A,
) -> Result<u64, WalkError> {
    let mut unmapped: u64 = 0;
    let mut addr = virt.as_u64();
    let pages = size / SIZE_4K;

    for _ in 0..pages {
        if let Some(_frame) = unmap_page(
            root,
            unsafe { CanonicalVirtAddr::new_unchecked(addr) },
            allocator,
        )? {
            unmapped += 1;
        }
        addr += SIZE_4K;
    }

    Ok(unmapped)
}
