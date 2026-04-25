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
        unsafe { $crate::page_table::utils::map_identity_range($mapper, $frame_allocator, stack_start_phys, stack_pages, $flags) }
            .expect("Failed to map current stack region");
        for desc in $memory_map.iter() {
            if desc.is_valid() {
                let start = desc.get_physical_start();
                let end = start + desc.get_page_count() * 4096;
                if rsp_phys >= start && rsp_phys < end && desc.get_page_count() <= $crate::page_table::constants::MAX_DESCRIPTOR_PAGES {
                    unsafe {
                        $crate::page_table::utils::map_identity_range(
                            $mapper,
                            $frame_allocator,
                            desc.get_physical_start(),
                            desc.get_page_count(),
                            $flags,
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
                let offset = match i {
                    1 => addr.as_u64() & 0x3FFFFFFF, // L3: 1GiB
                    2 => addr.as_u64() & 0x1FFFFF,   // L2: 2MiB
                    _ => panic!("Huge page at unexpected level: {}", i),
                };
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
            match map_huge_page(mapper, allocator, p_addr, v_addr, flags) {
                Ok(_) => {
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
            Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {},
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
            Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {},
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
    let l2_frame = l3[p3_idx].frame().expect("L2 frame should be present");
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

pub fn adjust_return_address_and_stack(phys_offset: VirtAddr) {
    debug_log_no_alloc!("Adjusting all return addresses and stack for higher half");
    unsafe {
        let mut rbp: u64;
        core::arch::asm!("mov {}, rbp", out(reg) rbp);
        if rbp != 0 {
            let mut current_rbp = rbp;
            loop {
                let frame_base_ptr = current_rbp as *mut u64;
                let next_rbp = frame_base_ptr.read();
                let return_address_ptr = frame_base_ptr.add(1);
                let old_return = return_address_ptr.read();
                return_address_ptr.write(old_return.wrapping_add(phys_offset.as_u64()));
                if next_rbp != 0 {
                    frame_base_ptr.write(next_rbp.wrapping_add(phys_offset.as_u64()));
                } else {
                    break;
                }
                current_rbp = next_rbp.wrapping_add(phys_offset.as_u64());
            }
        }
        let mut rsp: u64;
        core::arch::asm!("mov {}, rsp", out(reg) rsp);
        rsp = rsp.wrapping_add(phys_offset.as_u64());
        core::arch::asm!("mov rsp, {}", in(reg) rsp);
        core::arch::asm!("mov rbp, {}", in(reg) rbp.wrapping_add(phys_offset.as_u64()));
    }
    debug_log_no_alloc!("Return address and stack adjusted successfully");
}

pub fn map_stack_to_higher_half<T: crate::page_table::efi_memory::MemoryDescriptorValidator>(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
    memory_map: &[T],
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    let rsp = crate::get_current_stack_pointer!();
    for desc in memory_map.iter() {
        if desc.is_valid() {
            let start = desc.get_physical_start();
            let end = start + desc.get_page_count() * 4096;
            if rsp >= start && rsp < end {
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