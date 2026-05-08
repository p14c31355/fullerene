use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{
        FrameAllocator, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame,
        Size4KiB,
    },
};
use crate::page_table::constants::BootInfoFrameAllocator;

pub unsafe fn init(
    physical_memory_offset: VirtAddr,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    kernel_phys_start: u64,
) -> OffsetPageTable<'static> {
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [utils::init] entered\n");
    let level_4_table = unsafe { active_level_4_table(physical_memory_offset) };
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [utils::init] L4 table acquired\n");
    
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [utils::init] Creating OffsetPageTable\n");
    let mut mapper = unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) };
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [utils::init] OffsetPageTable created\n");
    
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [utils::init] Starting essential mappings\n");
    
    // CRITICAL: Map the first 1GB of physical memory to ensure that any page tables 
    // allocated by map_to are accessible. Using 2MB pages to avoid some huge page issues.
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [utils::init] Mapping first 1GB using 1GB pages\n");
    unsafe {
        // We map the first 1GB identity and higher-half.
        // Using 1GiB huge pages to ensure that any page tables 
        // allocated by map_to are accessible if they fall within this range.
        
        // Identity map first 64GB to ensure all allocated page tables are accessible
        let _ = crate::page_table::raw::map_range_with_1gib_pages(
            &mut mapper,
            frame_allocator,
            0,
            0,
            64,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        );

        // Higher-half map first 64GB
        let _ = crate::page_table::raw::map_range_with_1gib_pages(
            &mut mapper,
            frame_allocator,
            0,
            physical_memory_offset.as_u64(),
            64,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        );

        x86_64::instructions::tlb::flush_all();
    }
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [utils::init] 1GB huge pages mapped\n");

    // CRITICAL: Explicitly map the MEMORY_MAP_BUFFER to avoid page faults during heap search
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [utils::init] Mapping MEMORY_MAP_BUFFER\n");
    unsafe {
        let buffer_virt = VirtAddr::new(core::ptr::addr_of!(crate::page_table::heap::MEMORY_MAP_BUFFER) as u64);
        let buffer_phys_val = if buffer_virt >= physical_memory_offset {
            buffer_virt.as_u64() - physical_memory_offset.as_u64()
        } else {
            buffer_virt.as_u64()
        };
        let buffer_phys = PhysAddr::new(buffer_phys_val);
        let buffer_phys_virt = VirtAddr::new(buffer_phys_val);

        let buffer_size = core::mem::size_of::<[crate::page_table::memory_map::descriptor::EfiMemoryDescriptor; crate::page_table::heap::MAX_DESCRIPTORS]>();
        let pages = (buffer_size + 4095) / 4096;
        
        // Identity map
        let _ = mapper.map_to(
            Page::<Size4KiB>::containing_address(buffer_phys_virt),
            PhysFrame::<Size4KiB>::containing_address(buffer_phys),
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
            frame_allocator,
        );
        // High-half map
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [utils::init] Mapping MEMORY_MAP_BUFFER high-half start\n");
        let _ = mapper.map_to(
            Page::<Size4KiB>::containing_address(buffer_phys_virt + physical_memory_offset.as_u64()),
            PhysFrame::<Size4KiB>::containing_address(buffer_phys),
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
            frame_allocator,
        );
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [utils::init] Mapping MEMORY_MAP_BUFFER high-half done\n");
    }
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [utils::init] MEMORY_MAP_BUFFER mapped\n");

    let _boot_pages = crate::page_table::constants::BOOT_CODE_PAGES;
    
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [utils::init] Skipping redundant boot code mapping (covered by 1GB huge page)\n");
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [utils::init] Essential mappings completed\n");
    
    mapper
}

pub unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [active_level_4_table] entered\n");

    let cr3 = Cr3::read().0.start_address();
    let phys = cr3.as_u64();

    let virt = phys + physical_memory_offset.as_u64();
    
    let l4_ptr = virt as *mut PageTable;

    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [active_level_4_table] L4 virt addr: 0x");
    let mut buf = [0u8; 16];
    let len = crate::serial::format_hex_to_buffer(virt, &mut buf, 16);
    crate::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [active_level_4_table] dereferencing L4 ptr...\n");

    let table = &mut *l4_ptr;

    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [active_level_4_table] L4 table acquired successfully\n");
    table
}