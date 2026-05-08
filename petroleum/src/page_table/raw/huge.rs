use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame,
        Size4KiB,
    },
};

pub unsafe fn map_range_with_1gib_pages<A: FrameAllocator<Size4KiB>>(
    mapper: &mut OffsetPageTable,
    allocator: &mut A,
    phys: u64,
    virt: u64,
    gib_pages: u64,
    flags: PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    map_range_generic(
        mapper,
        allocator,
        phys,
        virt,
        gib_pages,
        1024 * 1024 * 1024,
        flags,
        "panic",
        |m, a, p, v, f| map_1gib_page(m, a, p, v, f),
    )
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
            crate::debug_log_no_alloc!(
                "Attempting huge page: phys=0x",
                p_addr as usize,
                " virt=0x",
                v_addr as usize
            );
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
        current_page += 1;
    }
    Ok(())
}

unsafe fn map_range_generic<A, F>(
    mapper: &mut OffsetPageTable,
    allocator: &mut A,
    phys: u64,
    virt: u64,
    pages: u64,
    page_size: u64,
    flags: PageTableFlags,
    behavior: &str,
    map_fn: F,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>>
where
    A: FrameAllocator<Size4KiB>,
    F: Fn(
        &mut OffsetPageTable,
        &mut A,
        u64,
        u64,
        PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>>,
{
    for i in 0..pages {
        let p_addr = phys + i * page_size;
        let v_addr = virt + i * page_size;
        match map_fn(mapper, allocator, p_addr, v_addr, flags) {
            Ok(_) => {}
            Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {
                // Ignore already mapped regions
            }
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
    let l4_entry_ptr = l4_ptr
        .cast::<x86_64::structures::paging::page_table::PageTableEntry>()
        .add(p4_idx.into());
    if !core::ptr::read(l4_entry_ptr)
        .flags()
        .contains(PageTableFlags::PRESENT)
    {
        let l3_frame = allocator
            .allocate_frame()
            .ok_or(x86_64::structures::paging::mapper::MapToError::FrameAllocationFailed)?;

        let mut entry = core::ptr::read(l4_entry_ptr);
        entry.set_addr(
            l3_frame.start_address(),
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        );
        core::ptr::write(l4_entry_ptr, entry);

        // Now that the L3 entry is set, the L3 table is mapped via phys_offset
        let l3_virt = mapper.phys_offset() + l3_frame.start_address().as_u64();
        unsafe {
            core::ptr::write_bytes(l3_virt.as_mut_ptr() as *mut u8, 0, 4096);
        }
    }
    let l3_frame = core::ptr::read(l4_entry_ptr)
        .frame()
        .expect("L3 frame should be present");
    let l3 = &mut *((mapper.phys_offset() + l3_frame.start_address().as_u64()).as_mut_ptr()
        as *mut PageTable);
    if l3[p3_idx].flags().contains(PageTableFlags::PRESENT) {
        return Err(
            x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(
                PhysFrame::containing_address(PhysAddr::new(phys)),
            ),
        );
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
    let l4_entry_ptr = l4_ptr
        .cast::<x86_64::structures::paging::page_table::PageTableEntry>()
        .add(p4_idx.into());
    if !core::ptr::read(l4_entry_ptr)
        .flags()
        .contains(PageTableFlags::PRESENT)
    {
        let l3_frame = allocator
            .allocate_frame()
            .ok_or(x86_64::structures::paging::mapper::MapToError::FrameAllocationFailed)?;
        let l3_virt = mapper.phys_offset() + l3_frame.start_address().as_u64();
        core::ptr::write_bytes(l3_virt.as_mut_ptr() as *mut u8, 0, 4096);
        let mut entry = core::ptr::read(l4_entry_ptr);
        entry.set_addr(
            l3_frame.start_address(),
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        );
        core::ptr::write(l4_entry_ptr, entry);
    }
    let l3_frame = core::ptr::read(l4_entry_ptr)
        .frame()
        .expect("L3 frame should be present");
    let l3 = &mut *((mapper.phys_offset() + l3_frame.start_address().as_u64()).as_mut_ptr()
        as *mut PageTable);
    if !l3[p3_idx].flags().contains(PageTableFlags::PRESENT) {
        let l2_frame = allocator
            .allocate_frame()
            .ok_or(x86_64::structures::paging::mapper::MapToError::FrameAllocationFailed)?;
        let l2_virt = mapper.phys_offset() + l2_frame.start_address().as_u64();
        core::ptr::write_bytes(l2_virt.as_mut_ptr() as *mut u8, 0, 4096);
        l3[p3_idx].set_addr(
            l2_frame.start_address(),
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        );
    }
    let l2_frame = match l3[p3_idx].frame() {
        Ok(f) => f,
        Err(x86_64::structures::paging::page_table::FrameError::HugeFrame) => {
            return Err(
                x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(
                    PhysFrame::containing_address(PhysAddr::new(phys)),
                ),
            );
        }
        Err(e) => panic!("Unexpected frame error in map_huge_page: {:?}", e),
    };
    let l2 = &mut *((mapper.phys_offset() + l2_frame.start_address().as_u64()).as_mut_ptr()
        as *mut PageTable);
    if l2[p2_idx].flags().contains(PageTableFlags::PRESENT) {
        return Err(
            x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(
                PhysFrame::containing_address(PhysAddr::new(phys)),
            ),
        );
    }
    l2[p2_idx].set_addr(PhysAddr::new(phys), flags | PageTableFlags::HUGE_PAGE);
    x86_64::instructions::tlb::flush_all();
    Ok(())
}
