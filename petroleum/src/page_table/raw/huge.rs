//! Huge page (2 MiB / 1 GiB) mapping utilities.
//!
//! These functions handle the special cases for huge page mappings,
//! including alignment checks and conflict detection.

use crate::page_table::types::*;
use crate::page_table::PageTableEntry;
use crate::page_table::raw::mapper::{map_huge_1g, map_huge_2m};
use crate::page_table::raw::walker::WalkError;
use crate::page_table::allocator::traits::FrameAllocator;

/// Errors specific to huge page operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HugeError {
    /// Virtual address is not aligned to the huge page boundary.
    VirtNotAligned { addr: u64, required: u64 },
    /// Physical address is not aligned to the huge page boundary.
    PhysNotAligned { addr: u64, required: u64 },
    /// A conflicting mapping already exists.
    Conflict { level: u8 },
    /// Out of memory.
    OutOfMemory,
}

impl core::fmt::Display for HugeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HugeError::VirtNotAligned { addr, required } => {
                write!(f, "virt 0x{:x} not aligned to 0x{:x}", addr, required)
            }
            HugeError::PhysNotAligned { addr, required } => {
                write!(f, "phys 0x{:x} not aligned to 0x{:x}", addr, required)
            }
            HugeError::Conflict { level } => {
                write!(f, "huge page conflict at level {}", level)
            }
            HugeError::OutOfMemory => write!(f, "out of memory"),
        }
    }
}

impl From<WalkError> for HugeError {
    fn from(e: WalkError) -> Self {
        match e {
            WalkError::OutOfMemory => HugeError::OutOfMemory,
            WalkError::HugePageConflict { level } => HugeError::Conflict { level },
            WalkError::InvalidEntry { level } => HugeError::Conflict { level },
        }
    }
}

/// Map a 2 MiB huge page with alignment checking.
pub fn map_2m_checked<A: FrameAllocator>(
    root: &mut PageTable,
    virt: CanonicalVirtAddr,
    phys: u64,
    flags: u64,
    allocator: &mut A,
) -> Result<(), HugeError> {
    if !virt.is_aligned(SIZE_2M) {
        return Err(HugeError::VirtNotAligned {
            addr: virt.as_u64(),
            required: SIZE_2M,
        });
    }
    if phys % SIZE_2M != 0 {
        return Err(HugeError::PhysNotAligned {
            addr: phys,
            required: SIZE_2M,
        });
    }

    map_huge_2m(root, virt, phys, flags, allocator)?;
    Ok(())
}

/// Map a 1 GiB huge page with alignment checking.
pub fn map_1g_checked<A: FrameAllocator>(
    root: &mut PageTable,
    virt: CanonicalVirtAddr,
    phys: u64,
    flags: u64,
    allocator: &mut A,
) -> Result<(), HugeError> {
    if !virt.is_aligned(SIZE_1G) {
        return Err(HugeError::VirtNotAligned {
            addr: virt.as_u64(),
            required: SIZE_1G,
        });
    }
    if phys % SIZE_1G != 0 {
        return Err(HugeError::PhysNotAligned {
            addr: phys,
            required: SIZE_1G,
        });
    }

    map_huge_1g(root, virt, phys, flags, allocator)?;
    Ok(())
}

/// Try to map with the largest possible page size.
///
/// Automatically selects 1 GiB, 2 MiB, or 4 KiB based on alignment.
pub fn map_auto<A: FrameAllocator>(
    root: &mut PageTable,
    virt: CanonicalVirtAddr,
    phys: u64,
    flags: u64,
    allocator: &mut A,
) -> Result<u64, HugeError> {
    let page_size = best_page_size(virt.as_u64(), phys, u64::MAX);

    match page_size {
        SIZE_1G => {
            map_1g_checked(root, virt, phys, flags, allocator)?;
            Ok(SIZE_1G)
        }
        SIZE_2M => {
            map_2m_checked(root, virt, phys, flags, allocator)?;
            Ok(SIZE_2M)
        }
        _ => {
            use crate::page_table::raw::mapper::map_page;
            let frame = PhysFrame::from_start_address(phys)
                .ok_or(HugeError::PhysNotAligned { addr: phys, required: SIZE_4K })?;
            map_page(root, virt, frame, flags, allocator)?;
            Ok(SIZE_4K)
        }
    }
}

/// Check if a huge page mapping would conflict with existing mappings.
pub fn check_huge_conflict(
    root: &PageTable,
    virt: CanonicalVirtAddr,
    level: u8,
) -> Result<(), HugeError> {
    let root_mut = unsafe { root.as_mut_for_walking() };
    let entry = crate::page_table::raw::walker::walk(root_mut, virt, level)?;

    if entry.is_present() {
        return Err(HugeError::Conflict { level });
    }

    Ok(())
}

// ── Backward-compat: map_range_with_huge_pages ────────────────────────
///
/// Maps a range using huge pages where possible, falling back to 4 KiB.
/// This is the function referenced by the legacy macro ecosystem.
///
/// # Safety
/// Caller must ensure mapper and allocator are valid.
pub unsafe fn map_range_with_huge_pages<A: x86_64::structures::paging::FrameAllocator<x86_64::structures::paging::Size4KiB>>(
    mapper: &mut x86_64::structures::paging::OffsetPageTable,
    allocator: &mut A,
    phys: u64,
    virt: u64,
    pages: u64,
    flags: x86_64::structures::paging::PageTableFlags,
    behavior: &str,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<x86_64::structures::paging::Size4KiB>> {
    use x86_64::structures::paging::{Page, PhysFrame, Size4KiB, Mapper};

    let mut current_page = 0;
    while current_page < pages {
        let p_addr = phys + current_page * 4096;
        let v_addr = virt + current_page * 4096;

        // Try 2 MiB huge page
        if p_addr % 0x200000 == 0 && v_addr % 0x200000 == 0 && (current_page + 512 <= pages) {
            let page = Page::<Size4KiB>::containing_address(x86_64::VirtAddr::new(v_addr));
            let frame = PhysFrame::<Size4KiB>::containing_address(x86_64::PhysAddr::new(p_addr));
            match mapper.map_to(page, frame, flags | x86_64::structures::paging::PageTableFlags::HUGE_PAGE, allocator) {
                Ok(flush) => { flush.flush(); current_page += 512; continue; }
                Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => { current_page += 512; continue; }
                Err(_) => {} // Fall through to 4K mapping
            }
        }

        // 4 KiB page
        let page = Page::<Size4KiB>::containing_address(x86_64::VirtAddr::new(v_addr));
        let frame = PhysFrame::<Size4KiB>::containing_address(x86_64::PhysAddr::new(p_addr));
        match mapper.map_to(page, frame, flags, allocator) {
            Ok(flush) => flush.flush(),
            Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {
                x86_64::instructions::tlb::flush(page.start_address());
            }
            Err(x86_64::structures::paging::mapper::MapToError::ParentEntryHugePage) => {}
            Err(e) => {
                if behavior == "panic" { panic!("Mapping error: {:?}", e); }
                return Err(e);
            }
        }
        current_page += 1;
    }
    Ok(())
}
