//! Page table translation: virtual → physical address lookup.
//!
//! Uses the unified walker for safe traversal.

use crate::page_table::types::*;
use crate::page_table::raw::walker::{walk, WalkError};

/// Translate a virtual address to a physical address.
///
/// Returns `Err` if the page table walk encounters a huge page conflict
/// or an unused entry.
pub fn translate(
    root: &PageTable,
    virt: CanonicalVirtAddr,
) -> Result<u64, WalkError> {
    // We need a mutable reference for the walker, but we only read.
    // This is safe because walk() never modifies entries.
    let root_mut = unsafe { root.as_mut_for_walking() };
    let entry = walk(root_mut, virt, 1)?;

    if !entry.is_present() {
        return Err(WalkError::OutOfMemory); // Entry not present
    }

    let page_offset = virt.page_offset_4k() as u64;
    Ok(entry.addr() + page_offset)
}

/// Translate a virtual address, returning the physical frame and offset.
pub fn translate_frame(
    root: &PageTable,
    virt: CanonicalVirtAddr,
) -> Result<(PhysFrame, u16), WalkError> {
    let root_mut = unsafe { root.as_mut_for_walking() };
    let entry = walk(root_mut, virt, 1)?;

    if !entry.is_present() {
        return Err(WalkError::OutOfMemory);
    }

    let frame = PhysFrame::from_start_address(entry.addr())
        .expect("page table entry has unaligned address");
    Ok((frame, virt.page_offset_4k()))
}

/// Translate a range of virtual addresses.
///
/// Returns a slice of physical addresses corresponding to each 4 KiB page
/// in the range.
pub fn translate_range(
    root: &PageTable,
    virt: CanonicalVirtAddr,
    size: u64,
) -> Result<heapless::Vec<u64, 64>, WalkError> {
    let mut result = heapless::Vec::new();
    let mut addr = virt.as_u64();
    let pages = core::cmp::min(size / SIZE_4K, 64);

    for _ in 0..pages {
        let phys = translate(root, unsafe { CanonicalVirtAddr::new_unchecked(addr) })?;
        result.push(phys).ok(); // Ignore if vec is full
        addr += SIZE_4K;
    }

    Ok(result)
}