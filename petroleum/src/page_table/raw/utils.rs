use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::Cr3,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame,
        Size4KiB,
    },
};

// Updated temporary virtual addresses for cloning and destroying page tables.
// These values now match the definitions in `constants.rs` and reside within the
// higher-half kernel address space, ensuring they are properly mapped during
// early boot and page table manipulation.
pub const TEMP_VA_FOR_CLONE: VirtAddr = VirtAddr::new(0xFFFF_9000_0000_0000);
pub const TEMP_VA_FOR_DESTROY: VirtAddr = VirtAddr::new(0xFFFF_A000_0000_0000);

/// Macro to safely map to higher half with logging
#[macro_export]
macro_rules! safe_map_to_higher_half {
    ($mapper:expr, $frame_allocator:expr, $phys_offset:expr, $phys_start:expr, $num_pages:expr, $flags:expr) => {{
        unsafe {
            $crate::page_table::raw::map_to_higher_half_with_log(
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
            $crate::page_table::raw::identity_map_range_with_log_macro!(
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
            x86_64::registers::control::Cr3::write(
                $frame,
                x86_64::registers::control::Cr3Flags::empty(),
            );
        }
    }};
}

/// Macro to safely read CR3
#[macro_export]
macro_rules! safe_cr3_read {
    () => {{ x86_64::registers::control::Cr3::read() }};
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
        crate::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: [with_temp_mapping] attempting map_to\n"
        );
        unsafe {
            let map_res = $mapper.map_to(
                page,
                $frame,
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                $frame_allocator,
            );
            crate::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: [with_temp_mapping] map_to returned\n"
            );
            match map_res {
                Ok(flush) => {
                    flush.flush();
                }
                Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {
                    crate::write_serial_bytes!(
                        0x3F8,
                        0x3FD,
                        b"DEBUG: [with_temp_mapping] PageAlreadyMapped, continuing\n"
                    );
                    x86_64::instructions::tlb::flush(page.start_address());
                }
                Err(e) => {
                    crate::write_serial_bytes!(
                        0x3F8,
                        0x3FD,
                        b"DEBUG: [with_temp_mapping] map_to failed\n"
                    );
                    return Err($crate::common::logging::SystemError::MappingFailed);
                }
            }
        }
        crate::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: [with_temp_mapping] map_to success and flushed\n"
        );
        let result = $body;
        crate::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: [with_temp_mapping] body executed, unmapping\n"
        );
        if let Ok((_frame, flush)) = $mapper.unmap(page) {
            flush.flush();
        }
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [with_temp_mapping] unmap success\n");
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
            $crate::page_table::raw::map_range_with_log_macro!(
                $mapper,
                $frame_allocator,
                $phys_start,
                $virt_start,
                $num_pages,
                $flags
            )
        }
        .unwrap_or_else(|_| panic!("Failed to map {} region", $desc))
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
                    let _ = $crate::page_table::raw::map_to_higher_half_with_log(
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
            $crate::page_table::raw::translate_addr(x86_64::VirtAddr::new(rsp_virt), phys_offset)
                .expect("Failed to translate RSP to physical address")
                .as_u64()
        };
        let stack_start_phys = rsp_phys & !4095;
        let stack_start_virt = rsp_virt & !4095;

        // Map current stack identity (for absolute safety during transition)
        unsafe {
            $crate::page_table::raw::map_range_4kiB(
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
            $crate::page_table::raw::map_range_4kiB(
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
                if rsp_phys >= start
                    && rsp_phys < end
                    && desc.get_page_count() <= $crate::page_table::constants::MAX_DESCRIPTOR_PAGES
                {
                    let virt_offset = rsp_virt - rsp_phys;
                    unsafe {
                        $crate::page_table::raw::map_range_4kiB(
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

pub unsafe fn map_identity_range(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    phys_start: u64,
    num_pages: u64,
    flags: PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    crate::page_table::raw::map_range_4kiB(
        mapper,
        frame_allocator,
        phys_start,
        phys_start,
        num_pages,
        flags,
        "panic",
    )
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
    Ok(())
}

pub fn calculate_phys_offset_diff(current: VirtAddr, new: VirtAddr) -> u64 {
    new.as_u64().wrapping_sub(current.as_u64())
}

pub fn adjust_return_address_and_stack(current_phys_offset: VirtAddr, new_phys_offset: VirtAddr) {
    crate::debug_log_no_alloc!("Adjusting current stack pointer for higher half");
    let offset_diff = calculate_phys_offset_diff(current_phys_offset, new_phys_offset);
    unsafe {
        let mut rbp: u64;
        core::arch::asm!("mov {}, rbp", out(reg) rbp);

        let mut rsp: u64;
        core::arch::asm!("mov {}, rsp", out(reg) rsp);
        rsp = rsp.wrapping_add(offset_diff);
        core::arch::asm!("mov rsp, {}", in(reg) rsp);
        core::arch::asm!("mov rbp, {}", in(reg) rbp.wrapping_add(offset_diff));
        crate::debug_log_no_alloc!("RSP/RBP adjusted");

        let rip: u64;
        core::arch::asm!("lea {}, [rip]", out(reg) rip);
        let target = rip.wrapping_add(offset_diff);
        crate::debug_log_no_alloc!("Jumping to target: 0x", target as usize);
        core::arch::asm!("jmp {}", in(reg) target);
    }
    crate::debug_log_no_alloc!("Stack pointer adjusted successfully");
}
