use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::Cr3,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame,
        Size4KiB, Translate,
    },
};
use crate::page_table::constants::BootInfoFrameAllocator;

pub const TEMP_VA_FOR_CLONE: VirtAddr = VirtAddr::new(0xffff_ffff_8000_0000);
pub const TEMP_VA_FOR_DESTROY: VirtAddr = VirtAddr::new(0xffff_ffff_9000_0000);

/// Macro to safely map to higher half with logging
#[macro_export]
macro_rules! safe_map_to_higher_half {
    ($mapper:expr, $frame_allocator:expr, $phys_offset:expr, $phys_start:expr, $num_pages:expr, $flags:expr) => {{
        unsafe {
            $crate::page_table::mapper::map_to_higher_half_with_log(
                $mapper,
                $frame_allocator,
                $phys_offset,
                $phys_start,
                $num_pages,
                $flags,
            )
        }
    }};
}

/// Macro to safely perform identity mapping with logging
#[macro_export]
macro_rules! safe_identity_map {
    ($mapper:expr, $frame_allocator:expr, $phys_start:expr, $num_pages:expr, $flags:expr) => {{
        unsafe {
            $crate::page_table::mapper::identity_map_range_with_log_macro!(
                $mapper,
                $frame_allocator,
                $phys_start,
                $num_pages,
                $flags
            )
        }
    }};
}

/// Macro to safely perform CR3 operations
#[macro_export]
macro_rules! safe_cr3_write {
    ($frame:expr) => {{
        unsafe {
            x86_64::registers::control::Cr3::write($frame, x86_64::registers::control::Cr3Flags::empty());
        }
    }};
}

/// Macro to safely read CR3
#[macro_export]
macro_rules! safe_cr3_read {
    () => {{
        x86_64::registers::control::Cr3::read()
    }};
}

/// Macro to consolidate CR3 read and validation operations
#[macro_export]
macro_rules! read_and_validate_cr3 {
    () => {{
        let (cr3_frame, _) = x86_64::registers::control::Cr3::read();
        $crate::debug_log_no_alloc!("CR3 read: 0x", cr3_frame.start_address().as_u64() as usize);
        cr3_frame
    }};
}

/// Macro to reduce repetitive TLB flush operations
#[macro_export]
macro_rules! flush_tlb_safely {
    () => {{
        let (current, flags) = x86_64::registers::control::Cr3::read();
        unsafe { x86_64::registers::control::Cr3::write(current, flags) };
        $crate::debug_log_no_alloc!("TLB flushed");
    }};
}

/// Macro to reduce repetitive temporary mapping operations
#[macro_export]
macro_rules! with_temp_mapping {
    ($mapper:expr, $frame_allocator:expr, $temp_va:expr, $frame:expr, $body:block) => {{
        let page = Page::<Size4KiB>::containing_address($temp_va);
        unsafe {
            $mapper
                .map_to(
                    page,
                    $frame,
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                    $frame_allocator,
                )
                .map_err(|_| $crate::common::logging::SystemError::MappingFailed)?
                .flush();
        }
        let result = $body;
        if let Ok((_frame, flush)) = $mapper.unmap(page) {
            flush.flush();
        }
        result
    }};
}

/// Macro to consolidate page table entry flag checks and frame extraction
#[macro_export]
macro_rules! extract_frame_if_present {
    ($entry:expr) => {
        if $entry.flags().contains(PageTableFlags::PRESENT) {
            $entry.frame().ok()
        } else {
            None
        }
    };
}

/// Macro to reduce repetitive mapping operations with logging
#[macro_export]
macro_rules! map_region_with_validation {
    ($mapper:expr, $frame_allocator:expr, $phys_start:expr, $virt_start:expr, $num_pages:expr, $flags:expr, $desc:expr) => {
        unsafe {
            $crate::page_table::mapper::map_range_with_log_macro!(
                $mapper,
                $frame_allocator,
                $phys_start,
                $virt_start,
                $num_pages,
                $flags
            )
        }.unwrap_or_else(|_| panic!("Failed to map {} region", $desc))
    };
}

/// Macro to consolidate memory descriptor mapping patterns using flag derivation functions
#[macro_export]
macro_rules! map_memory_with_flag_fn {
    ($mapper:expr, $frame_allocator:expr, $phys_offset:expr, $memory_map:expr, $filter_fn:expr, $flag_fn:expr) => {
        for desc in $memory_map.iter() {
            if desc.is_valid() && $filter_fn(desc) {
                let phys_start = desc.get_physical_start();
                let pages = desc.get_page_count();
                let flags = $flag_fn(desc);
                unsafe {
                    let _ = $crate::page_table::mapper::map_to_higher_half_with_log(
                        $mapper,
                        $frame_allocator,
                        $phys_offset,
                        phys_start,
                        pages,
                        flags,
                    );
                }
            }
        }
    };
}

/// Macro to get current stack pointer
#[macro_export]
macro_rules! get_current_stack_pointer {
    () => {{
        let rsp: u64;
        unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp); }
        rsp
    }};
}

/// Macro to map current stack
#[macro_export]
macro_rules! map_current_stack {
    ($mapper:expr, $frame_allocator:expr, $memory_map:expr, $flags:expr) => {{
        let rsp_virt = $crate::get_current_stack_pointer!();
        let stack_pages = 256; // 1MB stack
        let rsp_phys = unsafe {
            let (current_cr3, _) = x86_64::registers::control::Cr3::read();
            let phys_offset = x86_64::VirtAddr::new(0);
            $crate::page_table::utils::translate_addr(x86_64::VirtAddr::new(rsp_virt), phys_offset)
                .expect("Failed to translate RSP to physical address")
                .as_u64()
        };
        let stack_start_phys = rsp_phys & !4095;
        let stack_start_virt = rsp_virt & !4095;

        // Map current stack identity (for absolute safety during transition)
        unsafe {
            $crate::page_table::utils::map_range_4kiB(
                $mapper,
                $frame_allocator,
                stack_start_phys,
                stack_start_phys,
                stack_pages,
                $flags,
                "panic",
            )
        }
        .expect("Failed to map current stack identity");

        // Map current stack to its current virtual address
        unsafe {
            $crate::page_table::utils::map_range_4kiB(
                $mapper,
                $frame_allocator,
                stack_start_phys,
                stack_start_virt,
                stack_pages,
                $flags,
                "panic",
            )
        }
        .expect("Failed to map current stack virtual");

        for desc in $memory_map.iter() {
            if desc.is_valid() {
                let start = desc.get_physical_start();
                let end = start + desc.get_page_count() * 4096;
                if rsp_phys >= start && rsp_phys < end && desc.get_page_count() <= $crate::page_table::constants::MAX_DESCRIPTOR_PAGES {
                    let virt_offset = rsp_virt - rsp_phys;
                    unsafe {
                        $crate::page_table::utils::map_range_4kiB(
                            $mapper,
                            $frame_allocator,
                            desc.get_physical_start(),
                            desc.get_physical_start() + virt_offset,
                            desc.get_page_count(),
                            $flags,
                            "panic",
                        )
                    }
                    .expect("Failed to map stack region");
                    break;
                }
            }
        }
    }};
}

pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    let level_4_table = unsafe { active_level_4_table(physical_memory_offset) };
    unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) }
}

pub unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    let (level_4_table_frame, _) = Cr3::read();
    let phys = level_4_table_frame.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();
    unsafe { &mut *page_table_ptr }
}

pub unsafe fn translate_addr(addr: VirtAddr, physical_memory_offset: VirtAddr) -> Option<PhysAddr> {
    translate_addr_inner(addr, physical_memory_offset)
}

/// Calculates the offset within a huge page based on the page table level.
///
/// L3 (level 1 in the loop) corresponds to 1GiB pages.
/// L2 (level 2 in the loop) corresponds to 2MiB pages.
pub fn calculate_huge_page_offset(level: usize, addr: u64) -> u64 {
    match level {
        1 => addr & 0x3FFFFFFF, // L3: 1GiB
        2 => addr & 0x1FFFFF,   // L2: 2MiB
        _ => panic!("Huge page at unexpected level: {}", level),
    }
}

fn translate_addr_inner(addr: VirtAddr, physical_memory_offset: VirtAddr) -> Option<PhysAddr> {
    let (level_4_table_frame, _) = Cr3::read();
    let table_indexes = [
        addr.p4_index(),
        addr.p3_index(),
        addr.p2_index(),
        addr.p1_index(),
    ];
    let mut frame = level_4_table_frame;
    for (i, &index) in table_indexes.iter().enumerate() {
        let virt = physical_memory_offset + frame.start_address().as_u64();
        let table_ptr: *const PageTable = virt.as_ptr();
        let table = unsafe { &*table_ptr };
        let entry = &table[index];
        match entry.frame() {
            Ok(f) => frame = f,
            Err(x86_64::structures::paging::page_table::FrameError::FrameNotPresent) => return None,
            Err(x86_64::structures::paging::page_table::FrameError::HugeFrame) => {
                let phys_addr = entry.addr().as_u64();
                let offset = calculate_huge_page_offset(i, addr.as_u64());
                return Some(PhysAddr::new(phys_addr + offset));
            }
        }
    }
    Some(frame.start_address() + u64::from(addr.page_offset()))
}

pub fn create_example_mapping(
    page: Page,
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    let frame = PhysFrame::containing_address(PhysAddr::new(0xb8000));
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
    let map_to_result = unsafe { mapper.map_to(page, frame, flags, frame_allocator) };
    map_to_result.expect("map_to failed").flush();
}

pub unsafe fn map_range_with_1gib_pages<A: FrameAllocator<Size4KiB>>(
    mapper: &mut OffsetPageTable,
    allocator: &mut A,
    phys: u64,
    virt: u64,
    gib_pages: u64,
    flags: PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    for i in 0..gib_pages {
        let p_addr = phys + i * 1024 * 1024 * 1024;
        let v_addr = virt + i * 1024 * 1024 * 1024;
        unsafe {
            match map_1gib_page(mapper, allocator, p_addr, v_addr, flags) {
                Ok(_) => {},
                Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {
                    // Ignore already mapped regions to allow 4KiB mappings to take precedence
                },
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

pub unsafe fn map_range_with_huge_pages<A: FrameAllocator<Size4KiB>>(
    mapper: &mut OffsetPageTable,
    allocator: &mut A,
    phys: u64,
    virt: u64,
    pages: u64,
    flags: PageTableFlags,
    behavior: &str,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    let mut current_page = 0;
    while current_page < pages {
        let p_addr = phys + current_page * 4096;
        let v_addr = virt + current_page * 4096;
        if p_addr % 0x200000 == 0 && v_addr % 0x200000 == 0 && (current_page + 512 <= pages) {
            crate::debug_log_no_alloc!("Attempting huge page: phys=0x", p_addr as usize, " virt=0x", v_addr as usize);
            match map_huge_page(mapper, allocator, p_addr, v_addr, flags) {
                Ok(_) => {
                    crate::debug_log_no_alloc!("Huge page mapped successfully");
                    current_page += 512;
                    continue;
                }
                Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {}
                Err(e) => {
                    if behavior == "panic" {
                        panic!("Huge page mapping error: {:?}", e);
                    }
                    return Err(e);
                }
            }
        }
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(v_addr));
        let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(p_addr));
        match mapper.map_to(page, frame, flags, allocator) {
            Ok(flush) => flush.flush(),
            Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_frame)) => {
                // Update flags for already mapped page
                unsafe {
                    x86_64::instructions::tlb::flush(page.start_address());
                }
            }
            Err(x86_64::structures::paging::mapper::MapToError::ParentEntryHugePage) => {},
            Err(e) => {
                if behavior == "panic" {
                    panic!("Mapping error: {:?}", e);
                }
                return Err(e);
            }
        }
        current_page += 1;
    }
    Ok(())
}

pub unsafe fn map_range_4kiB<A: FrameAllocator<Size4KiB>>(
    mapper: &mut OffsetPageTable,
    allocator: &mut A,
    phys: u64,
    virt: u64,
    pages: u64,
    flags: PageTableFlags,
    behavior: &str,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    for i in 0..pages {
        let p_addr = phys + i * 4096;
        let v_addr = virt + i * 4096;
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(v_addr));
        let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(p_addr));
        match mapper.map_to(page, frame, flags, allocator) {
            Ok(flush) => flush.flush(),
            Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_frame)) => {
                // Update flags for already mapped page
                unsafe {
                    x86_64::instructions::tlb::flush(page.start_address());
                }
            }
            Err(x86_64::structures::paging::mapper::MapToError::ParentEntryHugePage) => {},
            Err(e) => {
                if behavior == "panic" {
                    panic!("Mapping error: {:?}", e);
                }
                return Err(e);
            }
        }
    }
    Ok(())
}

unsafe fn map_1gib_page<A: FrameAllocator<Size4KiB>>(
    mapper: &mut OffsetPageTable,
    allocator: &mut A,
    phys: u64,
    virt: u64,
    flags: PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    let l4_ptr = mapper.level_4_table() as *const PageTable as *mut PageTable;
    let p4_idx = VirtAddr::new(virt).p4_index();
    let p3_idx = VirtAddr::new(virt).p3_index();
    let l4_entry_ptr = unsafe { l4_ptr.cast::<x86_64::structures::paging::page_table::PageTableEntry>().add(p4_idx.into()) };
    if unsafe { !core::ptr::read(l4_entry_ptr).flags().contains(PageTableFlags::PRESENT) } {
        let l3_frame = allocator.allocate_frame().ok_or(x86_64::structures::paging::mapper::MapToError::FrameAllocationFailed)?;
        let l3_virt = mapper.phys_offset() + l3_frame.start_address().as_u64();
        core::ptr::write_bytes(l3_virt.as_mut_ptr() as *mut u8, 0, 4096);
        unsafe {
            let mut entry = core::ptr::read(l4_entry_ptr);
            entry.set_addr(l3_frame.start_address(), PageTableFlags::PRESENT | PageTableFlags::WRITABLE);
            core::ptr::write(l4_entry_ptr, entry);
        }
    }
    let l3_frame = unsafe { core::ptr::read(l4_entry_ptr).frame().expect("L3 frame should be present") };
    let l3 = &mut *((mapper.phys_offset() + l3_frame.start_address().as_u64()).as_mut_ptr() as *mut PageTable);
    if l3[p3_idx].flags().contains(PageTableFlags::PRESENT) {
        return Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(
            PhysFrame::containing_address(PhysAddr::new(phys)),
        ));
    }
    l3[p3_idx].set_addr(PhysAddr::new(phys), flags | PageTableFlags::HUGE_PAGE);
    x86_64::instructions::tlb::flush_all();
    Ok(())
}

unsafe fn map_huge_page<A: FrameAllocator<Size4KiB>>(
    mapper: &mut OffsetPageTable,
    allocator: &mut A,
    phys: u64,
    virt: u64,
    flags: PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    let l4_ptr = mapper.level_4_table() as *const PageTable as *mut PageTable;
    let p4_idx = VirtAddr::new(virt).p4_index();
    let p3_idx = VirtAddr::new(virt).p3_index();
    let p2_idx = VirtAddr::new(virt).p2_index();
    let l4_entry_ptr = unsafe { l4_ptr.cast::<x86_64::structures::paging::page_table::PageTableEntry>().add(p4_idx.into()) };
    if unsafe { !core::ptr::read(l4_entry_ptr).flags().contains(PageTableFlags::PRESENT) } {
        let l3_frame = allocator.allocate_frame().ok_or(x86_64::structures::paging::mapper::MapToError::FrameAllocationFailed)?;
        let l3_virt = mapper.phys_offset() + l3_frame.start_address().as_u64();
        core::ptr::write_bytes(l3_virt.as_mut_ptr() as *mut u8, 0, 4096);
        unsafe {
            let mut entry = core::ptr::read(l4_entry_ptr);
            entry.set_addr(l3_frame.start_address(), PageTableFlags::PRESENT | PageTableFlags::WRITABLE);
            core::ptr::write(l4_entry_ptr, entry);
        }
    }
    let l3_frame = unsafe { core::ptr::read(l4_entry_ptr).frame().expect("L3 frame should be present") };
    let l3 = &mut *((mapper.phys_offset() + l3_frame.start_address().as_u64()).as_mut_ptr() as *mut PageTable);
    if !l3[p3_idx].flags().contains(PageTableFlags::PRESENT) {
        let l2_frame = allocator.allocate_frame().ok_or(x86_64::structures::paging::mapper::MapToError::FrameAllocationFailed)?;
        let l2_virt = mapper.phys_offset() + l2_frame.start_address().as_u64();
        core::ptr::write_bytes(l2_virt.as_mut_ptr() as *mut u8, 0, 4096);
        l3[p3_idx].set_addr(l2_frame.start_address(), PageTableFlags::PRESENT | PageTableFlags::WRITABLE);
    }
    let l2_frame = match l3[p3_idx].frame() {
        Ok(f) => f,
        Err(x86_64::structures::paging::page_table::FrameError::HugeFrame) => {
            return Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(
                PhysFrame::containing_address(PhysAddr::new(phys)),
            ));
        }
        Err(e) => panic!("Unexpected frame error in map_huge_page: {:?}", e),
    };
    let l2 = &mut *((mapper.phys_offset() + l2_frame.start_address().as_u64()).as_mut_ptr() as *mut PageTable);
    if l2[p2_idx].flags().contains(PageTableFlags::PRESENT) {
        return Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(
            PhysFrame::containing_address(PhysAddr::new(phys)),
        ));
    }
    l2[p2_idx].set_addr(PhysAddr::new(phys), flags | PageTableFlags::HUGE_PAGE);
    x86_64::instructions::tlb::flush_all();
    Ok(())
}

pub unsafe fn map_identity_range(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    phys_start: u64,
    num_pages: u64,
    flags: PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    map_range_4kiB(mapper, frame_allocator, phys_start, phys_start, num_pages, flags, "panic")
}

pub fn debug_page_table_info(level_4_table_frame: PhysFrame, phys_offset: VirtAddr) {
    debug_log_no_alloc!(
        "New L4 table phys addr: ",
        level_4_table_frame.start_address().as_u64() as usize
    );
    debug_log_no_alloc!("Phys offset: ", phys_offset.as_u64() as usize);
}

/// Forcefully update flags for a given virtual address in the current page table.
pub unsafe fn force_update_page_flags(mapper: &mut OffsetPageTable, addr: VirtAddr, flags: PageTableFlags) {
    force_update_page_flags_no_flush(mapper, addr, flags);
    x86_64::instructions::tlb::flush(addr);
}

pub unsafe fn force_update_page_flags_no_flush(mapper: &mut OffsetPageTable, addr: VirtAddr, flags: PageTableFlags) {
    let p4_idx = addr.p4_index();
    let p3_idx = addr.p3_index();
    let p2_idx = addr.p2_index();
    let p1_idx = addr.p1_index();

    // 1. Update L4 entry
    let l4_ptr = mapper.level_4_table() as *const PageTable as *mut PageTable;
    let l4_entry_ptr = (l4_ptr as *mut x86_64::structures::paging::page_table::PageTableEntry).add(p4_idx.into());
    let l4_entry = unsafe { &mut *l4_entry_ptr };
    let mut l4_flags = l4_entry.flags();
    l4_flags.remove(PageTableFlags::NO_EXECUTE);
    l4_entry.set_flags(l4_flags);

    let l3_frame = l4_entry.frame().expect("L3 not present");
    let l3_ptr = (mapper.phys_offset() + l3_frame.start_address().as_u64()).as_mut_ptr() as *mut PageTable;
    
    // 2. Update L3 entry
    let l3_entry_ptr = (l3_ptr as *mut x86_64::structures::paging::page_table::PageTableEntry).add(p3_idx.into());
    let l3_entry = unsafe { &mut *l3_entry_ptr };
    let mut l3_flags = l3_entry.flags();
    l3_flags.remove(PageTableFlags::NO_EXECUTE);
    l3_entry.set_flags(l3_flags);

    if l3_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
        // This case is rare for L3 but handled for completeness
        l3_entry.set_flags(flags | PageTableFlags::HUGE_PAGE);
    } else {
        let l2_frame = l3_entry.frame().expect("L2 not present");
        let l2_ptr = (mapper.phys_offset() + l2_frame.start_address().as_u64()).as_mut_ptr() as *mut PageTable;
        
        let l2_entry_ptr = (l2_ptr as *mut x86_64::structures::paging::page_table::PageTableEntry).add(p2_idx.into());
        let l2_entry = unsafe { &mut *l2_entry_ptr };
        
        if l2_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
            // 3. Update L2 entry (Huge Page)
            l2_entry.set_flags(flags | PageTableFlags::HUGE_PAGE);
        } else {
            // 3. Update L2 entry (Pointer to L1)
            let mut l2_flags = l2_entry.flags();
            l2_flags.remove(PageTableFlags::NO_EXECUTE);
            l2_entry.set_flags(l2_flags);
            
            // 4. Update L1 entry
            let l1_frame = l2_entry.frame().expect("L1 not present");
            let l1_ptr = (mapper.phys_offset() + l1_frame.start_address().as_u64()).as_mut_ptr() as *mut PageTable;
            let l1_entry_ptr = (l1_ptr as *mut x86_64::structures::paging::page_table::PageTableEntry).add(p1_idx.into());
            unsafe { (*l1_entry_ptr).set_flags(flags) };
        }
    }
}

/// Calculates the difference between two physical memory offsets.
pub fn calculate_phys_offset_diff(current: VirtAddr, new: VirtAddr) -> u64 {
    new.as_u64().wrapping_sub(current.as_u64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use x86_64::VirtAddr;

    #[test]
    fn test_calculate_huge_page_offset_l3() {
        let addr = 0x1234_5678_9ABC_DEF0;
        let offset = calculate_huge_page_offset(1, addr);
        assert_eq!(offset, addr & 0x3FFFFFFF);
    }

    #[test]
    fn test_calculate_huge_page_offset_l2() {
        let addr = 0x1234_5678_9ABC_DEF0;
        let offset = calculate_huge_page_offset(2, addr);
        assert_eq!(offset, addr & 0x1FFFFF);
    }

    #[test]
    #[should_panic(expected = "Huge page at unexpected level")]
    fn test_calculate_huge_page_offset_invalid_level() {
        calculate_huge_page_offset(3, 0x1000);
    }

    #[test]
    fn test_calculate_phys_offset_diff() {
        let current = VirtAddr::new(0x1000);
        let new = VirtAddr::new(0x2000);
        assert_eq!(calculate_phys_offset_diff(current, new), 0x1000);
    }

    #[test]
    fn test_calculate_phys_offset_diff_wrapping() {
        let current = VirtAddr::new(0xFFFF_FFFF_FFFF_F000);
        let new = VirtAddr::new(0x0000_0000_0000_1000);
        // (0x1000 - 0xFF...F000) mod 2^64 = 0x2000
        assert_eq!(calculate_phys_offset_diff(current, new), 0x2000);
    }
}

pub fn adjust_return_address_and_stack(current_phys_offset: VirtAddr, new_phys_offset: VirtAddr) {
    debug_log_no_alloc!("Adjusting current stack pointer for higher half");
    let offset_diff = calculate_phys_offset_diff(current_phys_offset, new_phys_offset);
    unsafe {
        let mut rbp: u64;
        core::arch::asm!("mov {}, rbp", out(reg) rbp);
        
        let mut rsp: u64;
        core::arch::asm!("mov {}, rsp", out(reg) rsp);
        rsp = rsp.wrapping_add(offset_diff);
        core::arch::asm!("mov rsp, {}", in(reg) rsp);
        core::arch::asm!("mov rbp, {}", in(reg) rbp.wrapping_add(offset_diff));
        debug_log_no_alloc!("RSP/RBP adjusted");

        // The previous heuristic stack scanning logic was removed because it was 
        // dangerous and could cause silent data corruption by patching non-address 
        // values that happened to fall within the low-half kernel range.
        //
        // Transition to the higher half is now handled by ensuring the stack is 
        // correctly mapped and using a deterministic jump.
        debug_log_no_alloc!("Stack scanning skipped for safety");

        // Explicit jump to higher half to ensure rip is also transitioned immediately.
        // This prevents the CPU from executing in the low-half after CR3 switch.
        let rip: u64;
        core::arch::asm!("lea {}, [rip]", out(reg) rip);
        let target = rip.wrapping_add(offset_diff);
        debug_log_no_alloc!("Jumping to target: 0x", target as usize);
        core::arch::asm!("jmp {}", in(reg) target);
    }
    debug_log_no_alloc!("Stack pointer adjusted successfully");
}

pub fn map_stack_to_higher_half<T: crate::page_table::efi_memory::MemoryDescriptorValidator>(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
    current_phys_offset: VirtAddr,
    memory_map: &[T],
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    let rsp_virt = crate::get_current_stack_pointer!();
    let rsp_phys = unsafe {
        let (current_cr3, _) = x86_64::registers::control::Cr3::read();
        let phys_offset = x86_64::VirtAddr::new(0);
        crate::page_table::utils::translate_addr(x86_64::VirtAddr::new(rsp_virt), phys_offset)
            .expect("Failed to translate RSP to physical address")
            .as_u64()
    };
    for desc in memory_map.iter() {
        if desc.is_valid() {
            let start = desc.get_physical_start();
            let end = start + desc.get_page_count() * 4096;
            if rsp_phys >= start && rsp_phys < end {
                crate::safe_map_to_higher_half!(
                    mapper,
                    frame_allocator,
                    phys_offset,
                    desc.get_physical_start(),
                    desc.get_page_count(),
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE
                )?;
                break;
            }
        }
    }
    Ok(())
}