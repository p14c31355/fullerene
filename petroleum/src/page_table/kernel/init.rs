use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::Cr3,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame,
        Size4KiB,
    },
};

static PAGE_TABLE_INITIALIZED: AtomicBool = AtomicBool::new(false);
static mut STORED_OFFSET: Option<VirtAddr> = None;
static mut STORED_L4_PTR: Option<*mut PageTable> = None;

pub unsafe fn init(
    physical_memory_offset: VirtAddr,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    kernel_phys_start: u64,
) -> OffsetPageTable<'static> {
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
    // We must access page tables via identity (offset=0) because higher-half
    // mappings don't exist yet (or are from UEFI/bootloader and may not cover
    // the full range we need).
    let identity_offset = VirtAddr::zero();
    let level_4_table = unsafe { active_level_4_table(identity_offset) };
    crate::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [utils::init] L4 table acquired via identity\n"
    );

    // Create a temporary mapper with offset极0 for establishing the basic mappings
    let mut setup_mapper = unsafe { OffsetPageTable::new(level_4_table, identity_offset) };
    crate::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [utils::init] Setup mapper created (offset=0)\n"
    );

    crate::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [utils::init] Creating identity + higher-half 1GB mappings\n"
    );
    unsafe {
        // Identity map first 64GB using 1GB pages
        let _ = crate::page_table::raw::map_range_with_1gib_pages(
            &mut setup_mapper,
            frame_allocator,
            0,
            0,
            64,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        );

        // Higher-half map first 64GB at physical_memory_offset
        let _ = crate::page_table::raw::map_range_with_1gib_pages(
            &mut setup_mapper,
            frame_allocator,
            0,
            physical_memory_offset.as_u64(),
            64,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        );

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
    let l4_phys = Cr3::read().0.start_address();
    let l4_virt_addr = l4_phys.as_u64() + physical_memory_offset.as_u64();
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