use crate::page_table::types::*;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, Page, PageTableFlags, PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

pub const TEMP_VA_FOR_CLONE: VirtAddr = VirtAddr::new(0xFFFF_9000_0000_0000);
pub const TEMP_VA_FOR_DESTROY: VirtAddr = VirtAddr::new(0xFFFF_A000_0000_0000);

#[inline]
pub fn flush_tlb(virt: CanonicalVirtAddr) {
    x86_64::instructions::tlb::flush(VirtAddr::new(virt.as_u64()));
}

#[inline]
pub fn flush_tlb_all() {
    let (frame, flags) = x86_64::registers::control::Cr3::read();
    unsafe { x86_64::registers::control::Cr3::write(frame, flags) };
}

#[inline]
pub fn read_cr3() -> u64 {
    let (frame, _) = x86_64::registers::control::Cr3::read();
    frame.start_address().as_u64()
}

pub fn is_mapped(root: &PageTable, virt: CanonicalVirtAddr) -> bool {
    let root_mut = unsafe { root.as_mut_for_walking() };
    crate::page_table::raw::walker::walk(root_mut, virt, 1)
        .map(|e| e.is_present())
        .unwrap_or(false)
}

pub fn count_mapped(root: &PageTable, virt: CanonicalVirtAddr, size: u64) -> u64 {
    let mut count = 0u64;
    let mut addr = virt.as_u64();
    let page_count = (size + SIZE_4K - 1) / SIZE_4K;
    for _ in 0..page_count {
        if is_mapped(root, unsafe { CanonicalVirtAddr::new_unchecked(addr) }) {
            count += 1;
        }
        addr += SIZE_4K;
    }
    count
}

#[cfg(feature = "debug_pf")]
pub fn dump_entry(entry: &PageTableEntry, label: &str) {
    crate::serial_println!(
        "{}: addr=0x{:010x} flags=0x{:04x} ({}{}{}{}{}{})",
        label,
        entry.addr(),
        entry.flags(),
        if entry.is_present() { "P" } else { "-" },
        if entry.flags() & Flags::WRITABLE != 0 {
            "W"
        } else {
            "-"
        },
        if entry.flags() & Flags::USER_ACCESSIBLE != 0 {
            "U"
        } else {
            "-"
        },
        if entry.is_huge() { "H" } else { "-" },
        if entry.flags() & Flags::NO_EXECUTE != 0 {
            "NX"
        } else {
            "X"
        },
        if entry.flags() & Flags::GLOBAL != 0 {
            "G"
        } else {
            "-"
        },
    );
}

// ── Backward-compat mapping functions ────────────────────────

pub unsafe fn map_range_4kiB<A: FrameAllocator<Size4KiB>>(
    mapper: &mut x86_64::structures::paging::OffsetPageTable,
    allocator: &mut A,
    phys: u64,
    virt: u64,
    pages: u64,
    flags: PageTableFlags,
    behavior: &str,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> { unsafe {
    for i in 0..pages {
        let p_addr = phys + i * 4096;
        let v_addr = virt + i * 4096;
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(v_addr));
        let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(p_addr));
        match mapper.map_to(page, frame, flags, allocator) {
            Ok(flush) => flush.flush(),
            Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(
                existing_frame,
            )) => {
                // Check if the existing mapping matches the requested one
                if existing_frame != frame {
                    if behavior == "panic" {
                        panic!(
                            "Mapping error: existing frame {:?} differs from requested {:?}",
                            existing_frame, frame
                        );
                    } else {
                        return Err(
                            x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(
                                existing_frame,
                            ),
                        );
                    }
                } else {
                    x86_64::instructions::tlb::flush(page.start_address());
                }
            }
            Err(x86_64::structures::paging::mapper::MapToError::ParentEntryHugePage) => {}
            Err(e) if behavior == "panic" => panic!("Mapping error: {:?}", e),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}}

pub unsafe fn map_to_higher_half_with_log(
    mapper: &mut x86_64::structures::paging::OffsetPageTable,
    frame_allocator: &mut crate::page_table::constants::BootInfoFrameAllocator,
    phys_offset: VirtAddr,
    phys_start: u64,
    num_pages: u64,
    flags: PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> { unsafe {
    map_range_4kiB(
        mapper,
        frame_allocator,
        phys_start,
        phys_offset.as_u64() + phys_start,
        num_pages,
        flags,
        "panic",
    )
}}

pub unsafe fn map_identity_range(
    mapper: &mut x86_64::structures::paging::OffsetPageTable,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    phys_start: u64,
    num_pages: u64,
    flags: PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> { unsafe {
    map_range_4kiB(
        mapper,
        frame_allocator,
        phys_start,
        phys_start,
        num_pages,
        flags,
        "panic",
    )
}}

#[deprecated(note = "use map_identity_range")]
pub unsafe fn map_identity_range_checked(
    m: &mut x86_64::structures::paging::OffsetPageTable,
    a: &mut impl FrameAllocator<Size4KiB>,
    p: u64,
    n: u64,
    f: PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> { unsafe {
    map_identity_range(m, a, p, n, f)
}}

#[deprecated(note = "use map_range_4kiB")]
pub unsafe fn map_range_with_log_macro(
    m: &mut x86_64::structures::paging::OffsetPageTable,
    a: &mut impl FrameAllocator<Size4KiB>,
    p: u64,
    v: u64,
    n: u64,
    f: PageTableFlags,
    b: &str,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> { unsafe {
    map_range_4kiB(m, a, p, v, n, f, b)
}}

#[deprecated(note = "use map_to_higher_half_with_log")]
pub unsafe fn map_to_higher_half_with_log_macro(
    m: &mut x86_64::structures::paging::OffsetPageTable,
    fa: &mut crate::page_table::constants::BootInfoFrameAllocator,
    po: VirtAddr,
    ps: u64,
    np: u64,
    fl: PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> { unsafe {
    map_to_higher_half_with_log(m, fa, po, ps, np, fl)
}}

#[deprecated(note = "use map_range_4kiB")]
pub unsafe fn map_page_range(
    m: &mut x86_64::structures::paging::OffsetPageTable,
    a: &mut impl FrameAllocator<Size4KiB>,
    p: u64,
    v: u64,
    n: u64,
    f: PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> { unsafe {
    map_range_4kiB(m, a, p, v, n, f, "continue")
}}

#[deprecated(note = "use kernel::mapper::unmap_page")]
pub fn unmap_page_range(
    root: &mut crate::page_table::types::PageTable,
    virt: crate::page_table::types::CanonicalVirtAddr,
) -> Result<Option<crate::page_table::types::PhysFrame>, crate::page_table::raw::walker::WalkError>
{
    crate::page_table::kernel::mapper::unmap_page(
        root,
        virt,
        &mut crate::page_table::allocator::bitmap::BitmapFrameAllocator::new(0),
    )
}

#[deprecated]
pub fn get_memory_stats() -> (usize, usize, usize) {
    let allocator = crate::page_table::ALLOCATOR.lock();
    let used = allocator.used();
    let total = allocator.size();
    (used, total, total.saturating_sub(used))
}

#[deprecated(note = "use huge::map_range_with_huge_pages")]
pub unsafe fn map_range_with_huge_pages(
    m: &mut x86_64::structures::paging::OffsetPageTable,
    a: &mut impl FrameAllocator<Size4KiB>,
    p: u64,
    v: u64,
    n: u64,
    f: PageTableFlags,
    b: &str,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> { unsafe {
    crate::page_table::raw::huge::map_range_with_huge_pages(m, a, p, v, n, f, b)
}}

// ── Macros ──────────────────────────────────────────────────

#[macro_export]
macro_rules! extract_frame_if_present {
    ($entry:expr) => {
        if $entry
            .flags()
            .contains(x86_64::structures::paging::PageTableFlags::PRESENT)
        {
            $entry.frame().ok()
        } else {
            None
        }
    };
}

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

#[macro_export]
macro_rules! flush_tlb_safely {
    () => {{
        let (current, flags) = x86_64::registers::control::Cr3::read();
        unsafe { x86_64::registers::control::Cr3::write(current, flags) };
    }};
}

#[macro_export]
macro_rules! flush_tlb_and_verify {
    () => {{
        x86_64::instructions::tlb::flush_all();
        let (frame, flags) = x86_64::registers::control::Cr3::read();
        unsafe { x86_64::registers::control::Cr3::write(frame, flags) };
    }};
}

#[macro_export]
macro_rules! with_temp_mapping {
    ($mapper:expr, $frame_allocator:expr, $temp_va:expr, $frame:expr, $body:block) => {{
        let page = x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address($temp_va);
        unsafe {
            match $mapper.map_to(page, $frame,
                x86_64::structures::paging::PageTableFlags::PRESENT | x86_64::structures::paging::PageTableFlags::WRITABLE,
                $frame_allocator,
            ) {
                Ok(flush) => flush.flush(),
                Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {
                    x86_64::instructions::tlb::flush(page.start_address());
                }
                Err(_) => return Err($crate::common::logging::SystemError::MappingFailed),
            }
        }
        let result = $body;
        if let Ok((_frame, flush)) = $mapper.unmap(page) { flush.flush(); }
        result
    }};
}
