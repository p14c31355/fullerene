//! Utility functions for page table operations.

use crate::page_table::types::*;
use crate::page_table::PageTableEntry;
use x86_64::structures::paging::Mapper;

// ── Temporary virtual addresses for page table manipulation ────────────
//
// These are used by ProcessPageTable for temporary mappings during
// page table cloning and destruction.

/// Temporary virtual address for clone operations.
pub const TEMP_VA_FOR_CLONE: x86_64::VirtAddr = x86_64::VirtAddr::new(0xFFFF_9000_0000_0000);
/// Temporary virtual address for destroy operations.
pub const TEMP_VA_FOR_DESTROY: x86_64::VirtAddr = x86_64::VirtAddr::new(0xFFFF_A000_0000_0000);

/// Flush a single TLB entry for the given virtual address.
#[inline]
pub fn flush_tlb(virt: CanonicalVirtAddr) {
    unsafe {
        core::arch::asm!("invlpg [{}]", in(reg) virt.as_u64(), options(nomem, nostack));
    }
}

/// Flush the entire TLB by reloading CR3.
#[inline]
pub fn flush_tlb_all() {
    unsafe {
        let cr3: u64;
        core::arch::asm!("mov {}, cr3", out(reg) cr3);
        core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nomem, nostack));
    }
}

/// Read the current CR3 value (physical address of PML4).
#[inline]
pub fn read_cr3() -> u64 {
    unsafe {
        let cr3: u64;
        core::arch::asm!("mov {}, cr3", out(reg) cr3);
        cr3
    }
}

/// Check if a mapping exists for the given virtual address.
pub fn is_mapped(root: &PageTable, virt: CanonicalVirtAddr) -> bool {
    let root_mut = unsafe { root.as_mut_for_walking() };
    crate::page_table::raw::walker::walk(root_mut, virt, 1)
        .map(|e| e.is_present())
        .unwrap_or(false)
}

/// Count the number of mapped pages in a range.
pub fn count_mapped(root: &PageTable, virt: CanonicalVirtAddr, size: u64) -> u64 {
    let mut count = 0u64;
    let mut addr = virt.as_u64();
    let pages = size / SIZE_4K;

    for _ in 0..pages {
        if is_mapped(root, unsafe { CanonicalVirtAddr::new_unchecked(addr) }) {
            count += 1;
        }
        addr += SIZE_4K;
    }

    count
}

/// Dump a page table entry for debugging.
#[cfg(feature = "debug_pf")]
pub fn dump_entry(entry: &PageTableEntry, label: &str) {
    crate::serial_println!(
        "{}: addr=0x{:010x} flags=0x{:04x} ({}{}{}{}{}{})",
        label,
        entry.addr(),
        entry.flags(),
        if entry.is_present() { "P" } else { "-" },
        if entry.flags() & Flags::WRITABLE != 0 { "W" } else { "-" },
        if entry.flags() & Flags::USER_ACCESSIBLE != 0 { "U" } else { "-" },
        if entry.is_huge() { "H" } else { "-" },
        if entry.flags() & Flags::NO_EXECUTE != 0 { "NX" } else { "X" },
        if entry.flags() & Flags::GLOBAL != 0 { "G" } else { "-" },
    );
}

// ── Backward-compat functions for macro ecosystem ─────────────────────

/// Map a range of 4 KiB pages (backward-compat wrapper).
///
/// # Safety
/// Caller must ensure the mapper and allocator are valid.
pub unsafe fn map_range_4kiB<A: x86_64::structures::paging::FrameAllocator<x86_64::structures::paging::Size4KiB>>(
    mapper: &mut x86_64::structures::paging::OffsetPageTable,
    allocator: &mut A,
    phys: u64,
    virt: u64,
    pages: u64,
    flags: x86_64::structures::paging::PageTableFlags,
    behavior: &str,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<x86_64::structures::paging::Size4KiB>> {
    for i in 0..pages {
        let p_addr = phys + i * 4096;
        let v_addr = virt + i * 4096;
        let page = x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(x86_64::VirtAddr::new(v_addr));
        let frame = x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(x86_64::PhysAddr::new(p_addr));
        unsafe {
            match mapper.map_to(page, frame, flags, allocator) {
                Ok(flush) => flush.flush(),
                Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_frame)) => {
                    x86_64::instructions::tlb::flush(page.start_address());
                }
                Err(x86_64::structures::paging::mapper::MapToError::ParentEntryHugePage) => {}
                Err(e) => {
                    if behavior == "panic" {
                        panic!("Mapping error: {:?}", e);
                    }
                    return Err(e);
                }
            }
        }
    }
    Ok(())
}

/// Map to higher half with logging (backward-compat wrapper).
///
/// # Safety
/// Caller must ensure the mapper and allocator are valid.
pub unsafe fn map_to_higher_half_with_log(
    mapper: &mut x86_64::structures::paging::OffsetPageTable,
    frame_allocator: &mut crate::page_table::constants::BootInfoFrameAllocator,
    phys_offset: x86_64::VirtAddr,
    phys_start: u64,
    num_pages: u64,
    flags: x86_64::structures::paging::PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<x86_64::structures::paging::Size4KiB>> {
    let virt_start = phys_offset.as_u64() + phys_start;
    unsafe {
        map_range_4kiB(mapper, frame_allocator, phys_start, virt_start, num_pages, flags, "panic")?;
    }
    Ok(())
}

/// Identity-map a range (backward-compat wrapper).
///
/// # Safety
/// Caller must ensure the mapper and allocator are valid.
pub unsafe fn map_identity_range(
    mapper: &mut x86_64::structures::paging::OffsetPageTable,
    frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<x86_64::structures::paging::Size4KiB>,
    phys_start: u64,
    num_pages: u64,
    flags: x86_64::structures::paging::PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<x86_64::structures::paging::Size4KiB>> {
    unsafe {
        map_range_4kiB(mapper, frame_allocator, phys_start, phys_start, num_pages, flags, "panic")
    }
}

// ── Backward-compat function aliases ─────────────────────────────────

#[deprecated(note = "use map_identity_range")]
pub unsafe fn map_identity_range_checked(
    mapper: &mut x86_64::structures::paging::OffsetPageTable,
    frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<x86_64::structures::paging::Size4KiB>,
    phys_start: u64, num_pages: u64,
    flags: x86_64::structures::paging::PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<x86_64::structures::paging::Size4KiB>> {
    unsafe { map_identity_range(mapper, frame_allocator, phys_start, num_pages, flags) }
}

#[deprecated(note = "use map_range_4kiB")]
pub unsafe fn map_range_with_log_macro(
    mapper: &mut x86_64::structures::paging::OffsetPageTable,
    allocator: &mut impl x86_64::structures::paging::FrameAllocator<x86_64::structures::paging::Size4KiB>,
    phys: u64, virt: u64, pages: u64,
    flags: x86_64::structures::paging::PageTableFlags,
    behavior: &str,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<x86_64::structures::paging::Size4KiB>> {
    unsafe { map_range_4kiB(mapper, allocator, phys, virt, pages, flags, behavior) }
}

#[deprecated(note = "use map_to_higher_half_with_log")]
pub unsafe fn map_to_higher_half_with_log_macro(
    mapper: &mut x86_64::structures::paging::OffsetPageTable,
    frame_allocator: &mut crate::page_table::constants::BootInfoFrameAllocator,
    phys_offset: x86_64::VirtAddr,
    phys_start: u64, num_pages: u64,
    flags: x86_64::structures::paging::PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<x86_64::structures::paging::Size4KiB>> {
    unsafe { map_to_higher_half_with_log(mapper, frame_allocator, phys_offset, phys_start, num_pages, flags) }
}

#[deprecated(note = "use map_range_4kiB")]
pub unsafe fn map_page_range(
    mapper: &mut x86_64::structures::paging::OffsetPageTable,
    allocator: &mut impl x86_64::structures::paging::FrameAllocator<x86_64::structures::paging::Size4KiB>,
    phys: u64, virt: u64, pages: u64,
    flags: x86_64::structures::paging::PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<x86_64::structures::paging::Size4KiB>> {
    unsafe { map_range_4kiB(mapper, allocator, phys, virt, pages, flags, "continue") }
}

#[deprecated(note = "use kernel::mapper::unmap_page")]
pub fn unmap_page_range(
    root: &mut crate::page_table::types::PageTable,
    virt: crate::page_table::types::CanonicalVirtAddr,
) -> Result<Option<crate::page_table::types::PhysFrame>, crate::page_table::raw::walker::WalkError> {
    crate::page_table::kernel::mapper::unmap_page(root, virt, &mut crate::page_table::allocator::bitmap::BitmapFrameAllocator::new(0))
}

#[deprecated(note = "memory stats not available in new API")]
pub fn get_memory_stats() -> (usize, usize, usize) {
    (0, 0, 0)
}

#[deprecated(note = "use huge::map_range_with_huge_pages")]
pub unsafe fn map_range_with_huge_pages(
    mapper: &mut x86_64::structures::paging::OffsetPageTable,
    allocator: &mut impl x86_64::structures::paging::FrameAllocator<x86_64::structures::paging::Size4KiB>,
    phys: u64, virt: u64, pages: u64,
    flags: x86_64::structures::paging::PageTableFlags,
    behavior: &str,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<x86_64::structures::paging::Size4KiB>> {
    unsafe {
        crate::page_table::raw::huge::map_range_with_huge_pages(mapper, allocator, phys, virt, pages, flags, behavior)
    }
}

// ── Macros that were previously in this file ──────────────────────────

/// Macro to consolidate page table entry flag checks and frame extraction
#[macro_export]
macro_rules! extract_frame_if_present {
    ($entry:expr) => {
        if $entry.flags().contains(x86_64::structures::paging::PageTableFlags::PRESENT) {
            $entry.frame().ok()
        } else {
            None
        }
    };
}

/// Macro to safely perform CR3 write operations
#[macro_export]
macro_rules! safe_cr3_write {
    ($frame:expr) => {{
        unsafe {
            x86_64::registers::control::Cr3::write(
                $frame,
                x86_64::registers::control::Cr3Flags::empty(),
            );
        }
    }};
}

/// Macro to reduce repetitive TLB flush operations
#[macro_export]
macro_rules! flush_tlb_safely {
    () => {{
        let (current, flags) = x86_64::registers::control::Cr3::read();
        unsafe { x86_64::registers::control::Cr3::write(current, flags) };
    }};
}

/// Macro to flush TLB and verify
#[macro_export]
macro_rules! flush_tlb_and_verify {
    () => {{
        use x86_64::instructions::tlb;
        use x86_64::registers::control::{Cr3, Cr3Flags};
        tlb::flush_all();
        let (frame, flags): (
            x86_64::structures::paging::PhysFrame<x86_64::structures::paging::Size4KiB>,
            Cr3Flags,
        ) = Cr3::read();
        unsafe { Cr3::write(frame, flags) };
    }};
}

/// Macro to reduce repetitive temporary mapping operations
#[macro_export]
macro_rules! with_temp_mapping {
    ($mapper:expr, $frame_allocator:expr, $temp_va:expr, $frame:expr, $body:block) => {{
        let page = x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address($temp_va);
        unsafe {
            let map_res = $mapper.map_to(
                page,
                $frame,
                x86_64::structures::paging::PageTableFlags::PRESENT | x86_64::structures::paging::PageTableFlags::WRITABLE,
                $frame_allocator,
            );
            match map_res {
                Ok(flush) => {
                    flush.flush();
                }
                Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {
                    x86_64::instructions::tlb::flush(page.start_address());
                }
                Err(_) => {
                    return Err($crate::common::logging::SystemError::MappingFailed);
                }
            }
        }
        let result = $body;
        if let Ok((_frame, flush)) = $mapper.unmap(page) {
            flush.flush();
        }
        result
    }};
}
