use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::Cr3,
    structures::paging::{PageTable, PhysFrame, PageTableFlags},
};
use core::fmt::Write;

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

/// Dumps the page table walk for a given virtual address to the provided writer.
pub unsafe fn dump_page_table_walk(addr: VirtAddr, physical_memory_offset: VirtAddr, writer: &mut impl Write) {
    let (level_4_table_frame, _) = Cr3::read();
    let table_indexes = [
        addr.p4_index(),
        addr.p3_index(),
        addr.p2_index(),
        addr.p1_index(),
    ];
    let levels = ["L4", "L3", "L2", "L1"];
    
    let mut frame = level_4_table_frame;
    
    writeln!(writer, "Page Table Walk for {:#x}:", addr.as_u64()).ok();
    
    for (i, &index) in table_indexes.iter().enumerate() {
        let virt = physical_memory_offset + frame.start_address().as_u64();
        let table_ptr = virt.as_ptr() as *const PageTable;
        let table = &*table_ptr;
        let entry = &table[index];
        let flags = entry.flags();
        
        let entry_val = unsafe { *(entry as *const _ as *const u64) };
        writeln!(
            writer, 
            "  {} [index {:?}]: entry={:#x}, flags={:#x} (present={}, write={}, user={}, huge={})", 
            levels[i], 
            index, 
            entry_val, 
            flags.bits(), 
            flags.contains(PageTableFlags::PRESENT), 
            flags.contains(PageTableFlags::WRITABLE), 
            flags.contains(PageTableFlags::USER_ACCESSIBLE), 
            flags.contains(PageTableFlags::HUGE_PAGE)
        ).ok();

        if !flags.contains(PageTableFlags::PRESENT) {
            writeln!(writer, "  -> STOP: Entry not present").ok();
            return;
        }

        if flags.contains(PageTableFlags::HUGE_PAGE) {
            writeln!(writer, "  -> STOP: Huge page encountered").ok();
            return;
        }

        match entry.frame() {
            Ok(f) => frame = f,
            Err(_) => {
                writeln!(writer, "  -> STOP: Unexpected frame error").ok();
                return;
            }
        }
    }
    writeln!(writer, "  -> Walk completed successfully").ok();
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
            Err(x86_64::structures::paging::page_table::FrameError::FrameNotPresent) => {
                return None;
            }
            Err(x86_64::structures::paging::page_table::FrameError::HugeFrame) => {
                let phys_addr = entry.addr().as_u64();
                let offset = calculate_huge_page_offset(i, addr.as_u64());
                return Some(PhysAddr::new(phys_addr + offset));
            }
        }
    }
    Some(frame.start_address() + u64::from(addr.page_offset()))
}
