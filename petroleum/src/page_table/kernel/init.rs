//! Page table initialization and kernel jump logic.

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

/// Map a single 4KB page by inserting a L1 entry.
/// Uses allocate_frame() for all new tables since L1/L2/L3 tables may be >1MB.
/// `phys_offset` is used to convert physical addresses of page table entries
/// to virtual addresses for access (higher-half mapping).
pub unsafe fn map_page_4k_l1(
    l4: &mut PageTable,
    virt: VirtAddr,
    phys: PhysAddr,
    flags: PageTableFlags,
    frame_allocator: &mut crate::page_table::allocator::bitmap::BitmapFrameAllocator,
    phys_offset: VirtAddr,
) -> Result<(), &'static str> {
    let l4_idx = ((virt.as_u64() >> 39) & 0x1FF) as usize;
    let l3_idx = ((virt.as_u64() >> 30) & 0x1FF) as usize;
    let l2_idx = ((virt.as_u64() >> 21) & 0x1FF) as usize;
    let l1_idx = ((virt.as_u64() >> 12) & 0x1FF) as usize;

    let l3 = if l4[l4_idx].is_unused() {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or("4k: alloc L3 failed")?;
        let addr = frame.start_address();
        let ptr = (addr.as_u64() + phys_offset.as_u64()) as *mut u8;
        core::ptr::write_bytes(ptr, 0, 4096);
        l4[l4_idx].set_addr(addr, flags | PageTableFlags::PRESENT);
        &mut *((l4[l4_idx].addr().as_u64() + phys_offset.as_u64()) as *mut PageTable)
    } else {
        &mut *((l4[l4_idx].addr().as_u64() + phys_offset.as_u64()) as *mut PageTable)
    };

    let l2 = if l3[l3_idx].is_unused() {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or("4k: alloc L2 failed")?;
        let addr = frame.start_address();
        let ptr = (addr.as_u64() + phys_offset.as_u64()) as *mut u8;
        core::ptr::write_bytes(ptr, 0, 4096);
        l3[l3_idx].set_addr(addr, flags | PageTableFlags::PRESENT);
        &mut *((l3[l3_idx].addr().as_u64() + phys_offset.as_u64()) as *mut PageTable)
    } else {
        &mut *((l3[l3_idx].addr().as_u64() + phys_offset.as_u64()) as *mut PageTable)
    };

    // If the L2 entry is unused, allocate a new L1 table.
    // If it's a HUGE_PAGE, split it into 512 4KB entries.
    if l2[l2_idx].is_unused() {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or("4k: alloc L1 failed")?;
        let l1_phys = frame.start_address();
        let l1_ptr = (l1_phys.as_u64() + phys_offset.as_u64()) as *mut u8;
        core::ptr::write_bytes(l1_ptr, 0, 4096);
        l2[l2_idx].set_addr(l1_phys, flags | PageTableFlags::PRESENT);
    } else if l2[l2_idx].flags().contains(PageTableFlags::HUGE_PAGE) {
        let huge_page_phys_base = l2[l2_idx].addr().as_u64();
        let frame = frame_allocator
            .allocate_frame()
            .ok_or("4k: split L1 failed")?;
        let l1_phys = frame.start_address();
        let l1_ptr = (l1_phys.as_u64() + phys_offset.as_u64()) as *mut PageTable;
        core::ptr::write_bytes(l1_ptr as *mut u8, 0, 4096);
        let l1_ref = unsafe { &mut *l1_ptr };
        for j in 0..512u64 {
            l1_ref[j as usize].set_addr(PhysAddr::new(huge_page_phys_base + j * 4096), flags);
        }
        l2[l2_idx].set_addr(l1_phys, flags | PageTableFlags::PRESENT);
    }

    let l1_virt = (l2[l2_idx].addr().as_u64() + phys_offset.as_u64()) as *mut PageTable;
    let l1 = unsafe { &mut *l1_virt };
    l1[l1_idx].set_addr(phys, flags);
    Ok(())
}

/// Map a range of 4KB pages, using map_page_4k_l1 for each.
unsafe fn map_range_4k(
    l4: &mut PageTable,
    virt_start: VirtAddr,
    phys_start: PhysAddr,
    page_count: u64,
    flags: PageTableFlags,
    frame_allocator: &mut crate::page_table::allocator::bitmap::BitmapFrameAllocator,
    phys_offset: VirtAddr,
) -> Result<(), &'static str> {
    for i in 0..page_count {
        let virt = VirtAddr::new(virt_start.as_u64() + i * 4096);
        let phys = PhysAddr::new(phys_start.as_u64() + i * 4096);
        map_page_4k_l1(l4, virt, phys, flags, frame_allocator, phys_offset)?;
    }
    Ok(())
}

/// Map a range of 2MB huge pages by inserting a L2 huge-page entry.
/// Both virt_start and phys_start must be 2MB-aligned.
/// Uses frame_allocator (not allocate_frame_low) because huge page tables
/// can be allocated from anywhere in the first 16GB identity-mapped space.
unsafe fn map_range_2mb_huge(
    l4: &mut PageTable,
    virt_start: VirtAddr,
    phys_start: PhysAddr,
    page_count: u64,
    flags: PageTableFlags,
    frame_allocator: &mut crate::page_table::allocator::bitmap::BitmapFrameAllocator,
) -> Result<(), &'static str> {
    let flags_2mb = flags | PageTableFlags::HUGE_PAGE;
    for i in 0..page_count {
        let virt = VirtAddr::new(virt_start.as_u64() + i * 2 * 1024 * 1024);
        let phys = PhysAddr::new(phys_start.as_u64() + i * 2 * 1024 * 1024);
        let l4_idx = ((virt.as_u64() >> 39) & 0x1FF) as usize;
        let l3_idx = ((virt.as_u64() >> 30) & 0x1FF) as usize;
        let l2_idx = ((virt.as_u64() >> 21) & 0x1FF) as usize;

        if l4[l4_idx].is_unused() {
            // Allocate and zero a new L3 table
            if let Some(frame) = frame_allocator.allocate_frame() {
                let addr = frame.start_address();
                core::ptr::write_bytes(addr.as_u64() as *mut u8, 0, 4096);
                l4[l4_idx].set_addr(addr, flags | PageTableFlags::PRESENT);
            } else {
                return Err("huge: alloc L3 failed");
            }
        }

        let l3_addr = l4[l4_idx].addr();
        let l3 = &mut *(l3_addr.as_u64() as *mut PageTable);

        if l3[l3_idx].is_unused() {
            // Allocate and zero a new L2 table, then set the huge page entry
            if let Some(frame) = frame_allocator.allocate_frame() {
                let addr = frame.start_address();
                core::ptr::write_bytes(addr.as_u64() as *mut u8, 0, 4096);
                l3[l3_idx].set_addr(addr, flags | PageTableFlags::PRESENT);
                let l2 = &mut *(addr.as_u64() as *mut PageTable);
                l2[l2_idx].set_addr(phys, flags_2mb | PageTableFlags::PRESENT);
            } else {
                return Err("huge: alloc L2 failed");
            }
        } else {
            let addr = l3[l3_idx].addr();
            let l2 = &mut *(addr.as_u64() as *mut PageTable);
            l2[l2_idx].set_addr(phys, flags_2mb | PageTableFlags::PRESENT);
        }
    }
    Ok(())
}

/// Initialize page tables by creating a new L4 table and jumping to the kernel.
#[repr(C)]
pub struct InitAndJumpArgs {
    pub physical_memory_offset: VirtAddr,
    pub frame_allocator: *mut crate::page_table::allocator::bitmap::BitmapFrameAllocator,
    pub kernel_phys_start: u64,
    pub entry_virt: u64,
    pub stack_top: u64,
    pub arg1: u64,
    pub arg2: u64,
    pub map_phys_addr: u64,
    pub map_size: u64,
    pub l4_phys_addr: u64,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn init_and_jump(
    args_ptr: *const InitAndJumpArgs,
    stack_top_reg: u64,
    l4_phys_reg: u64,
    entry_virt_reg: usize,
    phys_offset_reg: u64,
) -> ! {
    let args = &*args_ptr;
    let physical_memory_offset = VirtAddr::new(phys_offset_reg);
    let frame_allocator = &mut *args.frame_allocator;
    let kernel_phys_start = args.kernel_phys_start;
    let entry_virt = entry_virt_reg;
    let stack_top = stack_top_reg;
    let arg1 = args.arg1;
    let arg2 = args.arg2;
    let map_phys_addr = args.map_phys_addr;
    let map_size = args.map_size;
    let l4_phys_addr = l4_phys_reg;

    crate::serial::_print(format_args!("IAJ: entered\n"));
    // Log the physical address of this function to verify it's within the identity map range
    let this_func_addr = init_and_jump as usize;
    crate::serial::_print(format_args!("IAJ: this_func_phys={:#x}\n", this_func_addr));

    // Based on the success pattern, reset the segment registers to clean the execution environment.
    unsafe {
        core::arch::asm!(
            "xor ax, ax",
            "mov ds, ax",
            "mov es, ax",
            "mov fs, ax",
            "mov gs, ax",
            "mov ss, ax",
            "and rsp, -16",
            options(preserves_flags)
        );
    }

    let current_rsp: usize;
    unsafe {
        core::arch::asm!("mov {}, rsp", out(reg) current_rsp);
    }
    crate::serial::_print(format_args!(
        "IAJ: args_ptr={:#x}, l4_phys={:#x}, stack_top={:#x}, current_rsp={:#x}\n",
        args_ptr as usize, l4_phys_addr, stack_top, current_rsp
    ));

    crate::serial::_print(format_args!("IAJ: Initializing L4 table...\n"));

    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_CACHE;

    // 1. Use the pre-allocated L4 table provided by the bootloader
    let l4_phys = l4_phys_addr;
    if l4_phys == 0 {
        crate::serial::_print(format_args!("IAJ: ERROR: l4_phys is NULL!\n"));
        loop {
            core::arch::asm!("hlt");
        }
    }

    let l4_ptr = l4_phys as *mut PageTable;

    crate::serial::_print(format_args!(
        "IAJ: Zeroing L4 table at {:#x}...\n",
        l4_ptr as usize
    ));
    unsafe {
        core::ptr::write_bytes(l4_ptr as *mut u8, 0, 4096);
    }
    let l4 = &mut *l4_ptr;

    crate::serial::_print(format_args!("IAJ: Using pre-allocated L4\n"));

    // STEP 1: Identity-map the entire 16GB physical address space using 2MB huge pages.
    // This must be done FIRST so that 4KB mappings can split specific huge pages later.
    crate::serial::_print(format_args!("IAJ: Mapping identity huge pages (16GB)...\n"));
    map_range_2mb_huge(
        l4,
        VirtAddr::new(0),
        PhysAddr::new(0),
        8192, // 16GB / 2MB
        flags,
        frame_allocator,
    )
    .expect("full 16GB huge page identity map failed");
    crate::serial::_print(format_args!("IAJ: Huge page identity mapped\n"));

    // STEP 1b: Map the entire 0-16GB physical range to the higher half using 2MB huge pages.
    // This MUST be done BEFORE any 4KB splits so that the 4KB functions can properly
    // split the higher-half huge pages.
    // This is CRITICAL: OffsetPageTable (used by map_to etc.) accesses page table
    // structures through phys_offset + table_phys_addr, and page tables can be
    // allocated at any physical address within 0-16GB by the frame allocator.
    crate::serial::_print(format_args!(
        "IAJ: Mapping full 16GB to higher half (huge pages)...\n"
    ));
    map_range_2mb_huge(
        l4,
        physical_memory_offset,
        PhysAddr::new(0),
        8192, // 16GB / 2MB
        flags,
        frame_allocator,
    )
    .expect("full 16GB huge page higher-half map failed");
    crate::serial::_print(format_args!("IAJ: Full higher-half mapping done\n"));

    // STEP 2: Split specific 2MB regions into 4KB pages for fine-grained mappings.
    // These replace the existing HUGE_PAGE entries with proper L1 tables.
    // The map_page_4k_l1 function handles HUGE_PAGE splitting automatically.

    // === Kernel mapping (higher-half + identity) ===
    crate::serial::_print(format_args!("IAJ: Mapping kernel...\n"));
    let kernel_size = 8 * 1024 * 1024u64; // Increase to 8MB
    let kernel_pages = kernel_size / 4096;

    // In init_and_jump, we are still running under UEFI's page table (before CR3 switch).
    // All page table structure accesses must use identity mapping (phys_offset = 0)
    // because the UEFI page table does NOT have higher-half mappings.
    // The new page table at `l4_phys` has both identity and higher-half mappings
    // (created by map_range_2mb_huge above), but we access it through identity mapping here.

    // higher-half kernel mapping (splits higher-half huge pages)
    let kernel_virt = VirtAddr::new(physical_memory_offset.as_u64() + kernel_phys_start);
    map_range_4k(
        l4,
        kernel_virt,
        PhysAddr::new(kernel_phys_start),
        kernel_pages,
        flags,
        frame_allocator,
        VirtAddr::new(0),
    )
    .expect("kernel higher map");

    // identity kernel mapping (splits identity huge pages)
    map_range_4k(
        l4,
        VirtAddr::new(kernel_phys_start),
        PhysAddr::new(kernel_phys_start),
        kernel_pages,
        flags,
        frame_allocator,
        VirtAddr::new(0),
    )
    .expect("kernel identity");
    crate::serial::_print(format_args!("IAJ: Kernel mapped\n"));

    // === Stack mapping (identity + higher-half) ===
    let stack_phys_page = (stack_top & 0x00000000_FFFFF000) as u64;
    crate::serial::_print(format_args!(
        "IAJ: Mapping stack... stack_top={:#x}, stack_phys_page={:#x}\n",
        stack_top, stack_phys_page
    ));
    let stack_pages = 8u64; // 32KB
    let stack_phys_base = stack_phys_page - stack_pages * 4096 + 4096;

    // identity
    map_range_4k(
        l4,
        VirtAddr::new(stack_phys_base),
        PhysAddr::new(stack_phys_base),
        stack_pages,
        flags,
        frame_allocator,
        VirtAddr::new(0),
    )
    .expect("stack identity");
    // higher-half
    map_range_4k(
        l4,
        VirtAddr::new(physical_memory_offset.as_u64() + stack_phys_base),
        PhysAddr::new(stack_phys_base),
        stack_pages,
        flags,
        frame_allocator,
        VirtAddr::new(0),
    )
    .expect("stack higher");
    crate::serial::_print(format_args!("IAJ: Stack mapped\n"));

    // Args (identity only - higher-half covered by huge page splits)
    let args_pages = 1u64;
    map_range_4k(
        l4,
        VirtAddr::new(arg1),
        PhysAddr::new(arg1),
        args_pages,
        flags,
        frame_allocator,
        VirtAddr::new(0),
    )
    .expect("args map");

    // Memory map (identity + higher-half)
    let map_pages = (map_size + 4095) / 4096;
    map_range_4k(
        l4,
        VirtAddr::new(map_phys_addr),
        PhysAddr::new(map_phys_addr),
        map_pages,
        flags,
        frame_allocator,
        VirtAddr::new(0),
    )
    .expect("map identity");
    let map_virt_higher = VirtAddr::new(physical_memory_offset.as_u64() + map_phys_addr);
    map_range_4k(
        l4,
        map_virt_higher,
        PhysAddr::new(map_phys_addr),
        map_pages,
        flags,
        frame_allocator,
        VirtAddr::new(0),
    )
    .expect("map higher");

    // Store state BEFORE switching CR3, since after the switch we can't safely reference
    // Rust statics (they may be in unmapped higher-half addresses)
    PAGE_TABLE_INITIALIZED.store(true, Ordering::SeqCst);
    STORED_OFFSET = Some(physical_memory_offset);
    STORED_L4_PTR = Some(l4_ptr);

    crate::serial::_print(format_args!("IAJ: mappings done, switching CR3...\n"));
    unsafe {
        x86_64::registers::control::Cr3::write(
            x86_64::structures::paging::PhysFrame::containing_address(PhysAddr::new(l4_phys)),
            x86_64::registers::control::Cr3Flags::empty(),
        )
    };
    x86_64::instructions::tlb::flush_all();

    // CRITICAL: After CR3 switch, we can only access identity-mapped addresses.
    // Do NOT call _print or any function that might reference unmapped code/sections.
    // Jump directly to kernel entry point.
    //
    // statics (PAGE_TABLE_INITIALIZED, STORED_OFFSET, STORED_L4_PTR) were set BEFORE the CR3 switch,
    // so we don't need to touch them now.

    // Jump to kernel entry point.
    // arg1 = page-aligned base of KernelArgs, arg2 = offset within that page.
    // Reconstruct the actual KernelArgs pointer: RDI = arg1 + arg2
    // RSI = physical_memory_offset (second argument to kernel)
    core::arch::asm!(
        "mov rsp, {stack}",
        "mov rax, {a1}",
        "add rax, {a2}",
        "mov rdi, rax",
        "mov rsi, {offset}",
        "jmp {entry}",
        stack = in(reg) stack_top,
        a1 = in(reg) arg1,
        a2 = in(reg) arg2,
        offset = in(reg) physical_memory_offset.as_u64(),
        entry = in(reg) entry_virt,
        options(noreturn),
    );
}

/// Legacy init function - kept for compatibility.
///
/// This always reuses the currently active page table (CR3) set up by init_and_jump
/// from the bootloader, creating an OffsetPageTable mapper for it.
pub unsafe fn init<A: FrameAllocator<Size4KiB>, F>(
    physical_memory_offset: VirtAddr,
    frame_allocator: &mut A,
    kernel_phys_start: u64,
    _early_mappings: Option<F>,
) -> OffsetPageTable<'static>
where
    F: FnOnce(&mut OffsetPageTable, &mut A),
{
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init] entered\n");

    // Store the statics for later reuse
    PAGE_TABLE_INITIALIZED.store(true, Ordering::SeqCst);
    STORED_OFFSET = Some(physical_memory_offset);

    // Get the current page table via CR3.
    // When called after init_and_jump, CR3 points to the L4 table set up by the bootloader,
    // which has both identity and higher-half mappings for 0-16GB.
    let (l4_frame, _) = Cr3::read();
    let l4_phys = l4_frame.start_address();
    let l4_higher_ptr = (l4_phys.as_u64() + physical_memory_offset.as_u64()) as *mut PageTable;

    STORED_L4_PTR = Some(l4_higher_ptr);

    let mapper = OffsetPageTable::new(&mut *l4_higher_ptr, physical_memory_offset);

    mapper
}

pub unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    let cr3 = Cr3::read().0.start_address();
    let phys = cr3.as_u64();
    let l4_ptr = (physical_memory_offset.as_u64() + phys) as *mut PageTable;
    &mut *l4_ptr
}
