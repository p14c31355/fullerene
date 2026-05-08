use crate::extract_frame_if_present;
use crate::page_table::types::PageTableHelper;
use alloc::collections::BTreeMap;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{
        FrameAllocator, OffsetPageTable, PageTable, PageTableFlags, PhysFrame, Size4KiB,
    },
};

pub fn clone_page_table(
    pt: &mut impl PageTableHelper,
    source_table: usize,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> crate::common::logging::SystemResult<usize> {
    // This logic is now integrated into ProcessPageTable via PageTableHelper,
    // but we can provide a standalone helper if needed.
    // For now, let's keep the implementation in table.rs as it's tightly coupled with ProcessPageTable's state.
    // However, the prompt asked for a separate clone.rs.
    // I will move the recursive logic here.
    Err(crate::common::logging::SystemError::NotImplemented)
}

pub unsafe fn clone_page_table_recursive<'a>(
    mapper: &mut OffsetPageTable<'a>,
    frame_alloc: &mut impl FrameAllocator<Size4KiB>,
    source_table_phys: PhysAddr,
    level: usize,
    allocated_frames: &mut alloc::vec::Vec<PhysFrame>,
    cloned_tables: &mut BTreeMap<PhysAddr, PhysAddr>,
) -> crate::common::logging::SystemResult<PhysAddr> {
    if level == 0 || level > 4 {
        return Err(crate::common::logging::SystemError::InvalidArgument);
    }

    if let Some(&cloned_phys) = cloned_tables.get(&source_table_phys) {
        return Ok(cloned_phys);
    }

    let dest_frame: PhysFrame = match frame_alloc.allocate_frame() {
        Some(frame) => frame,
        None => return Err(crate::common::logging::SystemError::FrameAllocationFailed),
    };
    let dest_phys = dest_frame.start_address();

    cloned_tables.insert(source_table_phys, dest_phys);

    let phys_offset = mapper.phys_offset();
    let source_va = phys_offset + source_table_phys.as_u64();
    let dest_va = phys_offset + dest_frame.start_address().as_u64();

    unsafe {
        let dest_ptr = dest_va.as_mut_ptr() as *mut u8;
        core::ptr::write_bytes(dest_ptr, 0, 4096);

        let source_table = &*(source_va.as_ptr() as *const PageTable);
        let dest_table = &mut *(dest_va.as_mut_ptr() as *mut PageTable);

        for (i, (source_entry, dest_entry)) in
            source_table.iter().zip(dest_table.iter_mut()).enumerate()
        {
            if source_entry.flags().contains(PageTableFlags::PRESENT) {
                if level > 1 && !source_entry.flags().contains(PageTableFlags::HUGE_PAGE) {
                    if let Some(child_frame) = extract_frame_if_present!(source_entry) {
                        let cloned_child_phys = clone_page_table_recursive(
                            mapper,
                            frame_alloc,
                            child_frame.start_address(),
                            level - 1,
                            allocated_frames,
                            cloned_tables,
                        )?;
                        dest_entry.set_addr(cloned_child_phys, source_entry.flags());
                    }
                } else {
                    dest_entry.set_addr(source_entry.addr(), source_entry.flags());
                }
            }
        }
    }

    allocated_frames.push(dest_frame);
    Ok(dest_frame.start_address())
}
