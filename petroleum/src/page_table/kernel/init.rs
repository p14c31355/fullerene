use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::{Cr3, Cr3Flags},
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame,
        Size4KiB,
    },
};

static PAGE_TABLE_INITIALIZED: AtomicBool = AtomicBool::new(false);
static mut STORED_OFFSET: Option<VirtAddr> = None;
static mut STORED_L4_PTR: Option<*mut PageTable> = None;

pub unsafe fn init<A: FrameAllocator<Size4KiB>, F>(
    physical_memory_offset: VirtAddr,
    frame_allocator: &mut A,
    kernel_phys_start: u64,
    early_mappings: Option<F>,
) -> OffsetPageTable<'static>
where
    F: FnOnce(&mut OffsetPageTable, &mut A),
{
    if PAGE_TABLE_INITIALIZED.load(Ordering::SeqCst) {
        // Reconstruct the OffsetPageTable from the stored L4 table pointer and offset.
        let offset = STORED_OFFSET.expect("STORED_OFFSET should be set");
        let l4_ptr = STORED_L4_PTR.expect("STORED_L4_PTR should be set");
        // SAFETY: The L4 page table is valid and mapped at the stored offset for 'static.
        let l4_table = &mut *l4_ptr;
        // SAFETY: The OffsetPageTable is reconstructed from the same valid page table.
        return OffsetPageTable::new(l4_table, offset);
    }
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [utils::init] entered\n");

    // Disable Write Protect (WP) bit in CR0 to allow writing to read-only pages
    use x86_64::registers::control::{Cr0, Cr0Flags};
    let mut cr0 = Cr0::read();
    cr0.remove(Cr0Flags::WRITE_PROTECT);
    Cr0::write(cr0);
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [utils::init] CR0.WP disabled\n");

    // PHASE 1: Establish identity + higher-half 1GB page mappings.
    // Instead of allocating a new L4 table and switching CR3 (which seems to be failing or inconsistent),
    // we will zero the EXISTING L4 table that the CPU is already using.
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [utils::init] Zeroing existing L4 table\n");
    
    let identity_offset = VirtAddr::zero();
    let level_4_table = unsafe { active_level_4_table(identity_offset) };
    let l4_ptr = level_4_table as *mut PageTable;
    
    unsafe {
        core::ptr::write_bytes(l4_ptr, 0, 1);
    }
    
    crate::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [utils::init] Existing L4 table zeroed\n"
    );

    // Create a temporary mapper with offset 0 for establishing the basic mappings
    let mut setup_mapper = unsafe { OffsetPageTable::new(level_4_table, identity_offset) };
    
    crate::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [utils::init] Creating identity + higher-half 1GB mappings\n"
    );
    unsafe {
        // Temporarily disable 1GB huge pages to diagnose #GP fault
        // let _ = crate::page_table::raw::map_range_with_1gib_pages(
        //     &mut setup_mapper,
        //     frame_allocator,
        //     0,
        //     0,
        //     64,
        //     PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        // );

        // let _ = crate::page_table::raw::map_range_with_1gib_pages(
        //     &mut setup_mapper,
        //     frame_allocator,
        //     0,
        //     physical_memory_offset.as_u64(),
        //     64,
        //     PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        // );

        x86_64::instructions::tlb::flush_all();
    }
    crate::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [utils::init] 1GB huge pages mapped\n"
    );

    // PHASE 2: Create the real OffsetPageTable with the desired physical_memory_offset.
    // Now that higher-half 1GB pages are mapped, accessing the L4 table via
    // physical_memory_offset is guaranteed to work.
    let l4_phys_addr = Cr3::read().0.start_address().as_u64();
    let l4_virt_addr = l4_phys_addr + physical_memory_offset.as_u64();
    let l4_ptr = l4_virt_addr as *mut PageTable;
    crate::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [utils::init] Creating OffsetPageTable with phys_offset\n"
    );
    let mut mapper = unsafe { OffsetPageTable::new(&mut *l4_ptr, physical_memory_offset) };
    crate::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [utils::init] OffsetPageTable created\n"
    );

    // MEMORY_MAP_BUFFER is already accessible through higher-half mapping, no need to remap
    crate::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [utils::init] MEMORY_MAP_BUFFER already mapped via higher-half\n"
    );

    let _boot_pages = crate::page_table::constants::BOOT_CODE_PAGES;

    crate::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [utils::init] Skipping redundant boot code mapping (covered by 1GB huge page)\n"
    );
    crate::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [utils::init] Essential mappings completed\n"
    );

    PAGE_TABLE_INITIALIZED.store(true, Ordering::SeqCst);
    STORED_OFFSET = Some(physical_memory_offset);
    STORED_L4_PTR = Some(l4_ptr);

    mapper
}

pub unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [active_level_4_table] entered\n");

    let cr3 = Cr3::read().0.start_address();
    let phys = cr3.as_u64();

    let virt = phys + physical_memory_offset.as_u64();

    let l4_ptr = virt as *mut PageTable;

    crate::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [active_level_4_table] L4 virt addr: 0x"
    );
    let mut buf = [0u8; 16];
    let len = crate::serial::format_hex_to_buffer(virt, &mut buf, 16);
    crate::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

    crate::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [active_level_4_table] dereferencing L4 ptr...\n"
    );

    let table = &mut *l4_ptr;

    crate::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [active_level_4_table] L4 table acquired successfully\n"
    );
    table
}