use x86_64::{
    VirtAddr,
    structures::paging::{
        OffsetPageTable, FrameAllocator, Size4KiB, Page, PhysFrame, PageTableFlags, Mapper,
    },
};
use crate::page_table::constants::HIGHER_HALF_OFFSET;
use crate::page_table::memory_map::descriptor::MemoryMapDescriptor;

/// Initializes a direct physical mapping of all usable physical memory to the higher half.
/// 
/// This function maps physical memory 1:1 starting from `HIGHER_HALF_OFFSET`.
/// It prioritizes 2MiB huge pages to reduce page table overhead and improve performance.
pub fn init_direct_physical_mapping(
    memory_map: &[MemoryMapDescriptor],
    allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<OffsetPageTable<'static>, &'static str> {
    let pml4_frame = allocator.allocate_frame()
        .ok_or("Failed to allocate PML4 frame")?;
    
    let pml4_virt = HIGHER_HALF_OFFSET + pml4_frame.start_address().as_u64();
    unsafe {
        core::ptr::write_bytes(pml4_virt.as_ptr::<u8>() as *mut u8, 0, 4096);
    }

    let mut mapper = unsafe {
        let pml4_ptr = pml4_virt.as_ptr::<x86_64::structures::paging::PageTable>() as *mut x86_64::structures::paging::PageTable;
        let mapper = OffsetPageTable::new(&mut *pml4_ptr, HIGHER_HALF_OFFSET);

        core::mem::transmute::<OffsetPageTable<'_>, OffsetPageTable<'static>>(mapper)
    };

    for desc in memory_map {
        if desc.type_() == crate::common::EfiMemoryType::EfiConventionalMemory as u32 {
            let phys_start = desc.physical_start();
            let pages = desc.number_of_pages();
            let size = pages * 4096;
            
            let mut current_phys = phys_start;
            let end_phys = phys_start + size;

            while current_phys < end_phys {
                let remaining = end_phys - current_phys;
                
                if current_phys % (2 * 1024 * 1024) == 0 && remaining >= (2 * 1024 * 1024) {
                    let virt_addr = VirtAddr::new(HIGHER_HALF_OFFSET.as_u64() + current_phys);
                    let page = Page::<Size4KiB>::containing_address(virt_addr);
                    let frame = PhysFrame::containing_address(x86_64::PhysAddr::new(current_phys));
                    
                    unsafe {
                        mapper.map_to(
                            page,
                            frame,
                            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::HUGE_PAGE,
                            allocator,
                        ).map_err(|_| "Failed to map huge page")?.flush();
                    }
                    current_phys += 2 * 1024 * 1024;
                } else {
                    let virt_addr = VirtAddr::new(HIGHER_HALF_OFFSET.as_u64() + current_phys);
                    let page = Page::<Size4KiB>::containing_address(virt_addr);
                    let frame = PhysFrame::containing_address(x86_64::PhysAddr::new(current_phys));
                    
                    unsafe {
                        mapper.map_to(
                            page,
                            frame,
                            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                            allocator,
                        ).map_err(|_| "Failed to map 4KiB page")?.flush();
                    }
                    current_phys += 4096;
                }
            }
        }
    }

    Ok(mapper)
}
