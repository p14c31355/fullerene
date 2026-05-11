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

/// Write a string to VGA buffer for debugging (no serial port needed)
unsafe fn vga_write(s: &[u8]) {
    let vga = 0xb8000 as *mut u16;
    static mut ROW: usize = 0;
    static mut COL: usize = 0;
    for &byte in s {
        if byte == b'\n' {
            ROW += 1;
            COL = 0;
            continue;
        }
        if ROW < 25 && COL < 80 {
            vga.add(ROW * 80 + COL).write_volatile(byte as u16 | 0x0F00);
            COL += 1;
        }
    }
}

/// Map a single 4KB page in the existing UEFI page table by directly modifying entries.
unsafe fn map_page_4k_existing(
    l4: &mut PageTable,
    virt: VirtAddr,
    phys: PhysAddr,
    flags: PageTableFlags,
    frame_allocator: &mut crate::page_table::allocator::bitmap::BitmapFrameAllocator,
) -> Result<(), &'static str> {
    let l4_idx = ((virt.as_u64() >> 39) & 0x1FF) as usize;
    let l3_idx = ((virt.as_u64() >> 30) & 0x1FF) as usize;
    let l2_idx = ((virt.as_u64() >> 21) & 0x1FF) as usize;
    let l1_idx = ((virt.as_u64() >> 12) & 0x1FF) as usize;

    let l3_phys = if l4[l4_idx].is_unused() {
        let frame = frame_allocator.allocate_frame_low().ok_or("alloc L3 failed")?;
        let addr = frame.start_address();
        l4[l4_idx].set_addr(addr, flags | PageTableFlags::PRESENT);
        addr
    } else {
        l4[l4_idx].addr()
    };
    let l3 = &mut *(l3_phys.as_u64() as *mut PageTable);

    let l2_phys = if l3[l3_idx].is_unused() {
        let frame = frame_allocator.allocate_frame_low().ok_or("alloc L2 failed")?;
        let addr = frame.start_address();
        l3[l3_idx].set_addr(addr, flags | PageTableFlags::PRESENT);
        addr
    } else {
        l3[l3_idx].addr()
    };
    let l2 = &mut *(l2_phys.as_u64() as *mut PageTable);

    let l1_phys = if l2[l2_idx].is_unused() {
        let frame = frame_allocator.allocate_frame_low().ok_or("alloc L1 failed")?;
        let addr = frame.start_address();
        l2[l2_idx].set_addr(addr, flags | PageTableFlags::PRESENT);
        addr
    } else {
        l2[l2_idx].addr()
    };
    let l1 = &mut *(l1_phys.as_u64() as *mut PageTable);

    l1[l1_idx].set_addr(phys, flags);
    Ok(())
}

/// Map a range of 4KB pages in the existing UEFI page table.
unsafe fn map_range_4k_existing(
    l4: &mut PageTable,
    virt_start: VirtAddr,
    phys_start: PhysAddr,
    page_count: u64,
    flags: PageTableFlags,
    frame_allocator: &mut crate::page_table::allocator::bitmap::BitmapFrameAllocator,
) -> Result<(), &'static str> {
    for i in 0..page_count {
        let virt = VirtAddr::new(virt_start.as_u64() + i * 4096);
        let phys = PhysAddr::new(phys_start.as_u64() + i * 4096);
        map_page_4k_existing(l4, virt, phys, flags, frame_allocator)?;
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
pub unsafe extern "C" fn init_and_jump(args: *const InitAndJumpArgs) -> ! {
    let args = &*args;
    let physical_memory_offset = args.physical_memory_offset;
    let frame_allocator = &mut *args.frame_allocator;
    let kernel_phys_start = args.kernel_phys_start;
    let entry_virt = args.entry_virt;
    let stack_top = args.stack_top;
    let arg1 = args.arg1;
    let arg2 = args.arg2;
    let map_phys_addr = args.map_phys_addr;
    let map_size = args.map_size;
    let l4_phys_addr = args.l4_phys_addr;

    vga_write(b"IAJ: entered\n");

    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

    // 1. Use the pre-allocated L4 table provided by the bootloader
    let l4_phys = l4_phys_addr;
    let l4_ptr = l4_phys as *mut PageTable;
    core::ptr::write_bytes(l4_ptr, 0, 1);
    let l4 = &mut *l4_ptr;

    vga_write(b"IAJ: Using pre-allocated L4\n");

    // === CRITICAL: Identity map bootloader code area (0-64MB) to prevent #PF after CR3 switch ===
    if let Err(e) = map_range_4k_existing(
        l4,
        VirtAddr::new(0),
        PhysAddr::new(0),
        16384, // 64MB
        flags,
        frame_allocator,
    ) {
        vga_write(b"IAJ: ERROR mapping bootloader identity\n");
        loop { core::arch::asm!("hlt"); }
    }

    // Also identity map the L4 table itself so we can access it if needed
    map_page_4k_existing(l4, VirtAddr::new(l4_phys), PhysAddr::new(l4_phys), flags, frame_allocator)
        .expect("L4 identity map failed");

    // === Kernel mapping (higher-half + identity) ===
    let kernel_size = 8 * 1024 * 1024u64; // Increase to 8MB
    let kernel_pages = kernel_size / 4096;

    // higher-half
    let kernel_virt = VirtAddr::new(physical_memory_offset.as_u64() + kernel_phys_start);
    map_range_4k_existing(l4, kernel_virt, PhysAddr::new(kernel_phys_start), kernel_pages, flags, frame_allocator)
        .expect("kernel higher map");

    // identity
    map_range_4k_existing(l4, VirtAddr::new(kernel_phys_start), PhysAddr::new(kernel_phys_start), kernel_pages, flags, frame_allocator)
        .expect("kernel identity");

    // === Stack mapping (identity + higher-half) ===
    let stack_pages = 8u64; // 32KB
    let stack_phys_base = stack_top - stack_pages * 4096 + 4096;
    
    map_range_4k_existing(l4, VirtAddr::new(stack_phys_base), PhysAddr::new(stack_phys_base), stack_pages, flags, frame_allocator)
        .expect("stack identity");

    map_range_4k_existing(
        l4,
        VirtAddr::new(physical_memory_offset.as_u64() + stack_phys_base),
        PhysAddr::new(stack_phys_base),
        stack_pages,
        flags,
        frame_allocator,
    ).expect("stack higher");

    // Args, Memory Map, Low memory higher-half
    let args_pages = 1u64;
    map_range_4k_existing(l4, VirtAddr::new(arg1), PhysAddr::new(arg1), args_pages, flags, frame_allocator)
        .expect("args map");

    let map_pages = (map_size + 4095) / 4096;
    map_range_4k_existing(l4, VirtAddr::new(map_phys_addr), PhysAddr::new(map_phys_addr), map_pages, flags, frame_allocator)
        .expect("map identity");

    let map_virt_higher = VirtAddr::new(physical_memory_offset.as_u64() + map_phys_addr);
    map_range_4k_existing(l4, map_virt_higher, PhysAddr::new(map_phys_addr), map_pages, flags, frame_allocator)
        .expect("map higher");

    // Map first 16MB of physical memory to higher half
    map_range_4k_existing(
        l4,
        physical_memory_offset,
        PhysAddr::new(0),
        4096, // 16MB / 4KB
        flags,
        frame_allocator,
    ).expect("low memory higher map");

    // Map the new L4 table itself to the higher half
    let l4_virt_higher = VirtAddr::new(physical_memory_offset.as_u64() + l4_phys);
    map_page_4k_existing(l4, l4_virt_higher, PhysAddr::new(l4_phys), flags, frame_allocator).ok();

    vga_write(b"IAJ: mappings done, switching CR3...\n");

    unsafe { x86_64::registers::control::Cr3::write(
        x86_64::structures::paging::PhysFrame::containing_address(PhysAddr::new(l4_phys)),
        x86_64::registers::control::Cr3Flags::empty(),
    ) };
    x86_64::instructions::tlb::flush_all();

    vga_write(b"IAJ: CR3 switched, jumping!\n");

    // Store state
    PAGE_TABLE_INITIALIZED.store(true, Ordering::SeqCst);
    STORED_OFFSET = Some(physical_memory_offset);
    STORED_L4_PTR = Some(l4_ptr);

    // Jump to kernel entry point
    core::arch::asm!(
        "mov rsp, {stack}",
        "mov rdi, {a1}",
        "mov rsi, {a2}",
        "jmp {entry}",
        stack = in(reg) stack_top,
        a1 = in(reg) arg1,
        a2 = in(reg) arg2,
        entry = in(reg) entry_virt,
        options(noreturn),
    );
}

/// Legacy init function - kept for compatibility
pub unsafe fn init<A: FrameAllocator<Size4KiB>, F>(
    physical_memory_offset: VirtAddr,
    frame_allocator: &mut A,
    kernel_phys_start: u64,
    _early_mappings: Option<F>,
) -> OffsetPageTable<'static>
where
    F: FnOnce(&mut OffsetPageTable, &mut A),
{
    if PAGE_TABLE_INITIALIZED.load(Ordering::SeqCst) {
        let offset = STORED_OFFSET.expect("STORED_OFFSET should be set");
        let l4_ptr = STORED_L4_PTR.expect("STORED_L4_PTR should be set");
        let l4_table = &mut *l4_ptr;
        return OffsetPageTable::new(l4_table, offset);
    }

    crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init] entered\n");

    use x86_64::registers::control::{Cr0, Cr0Flags};
    let mut cr0 = Cr0::read();
    cr0.remove(Cr0Flags::WRITE_PROTECT);
    Cr0::write(cr0);

    let l4_frame = frame_allocator
        .allocate_frame()
        .expect("Failed to allocate L4 table");
    let l4_phys_addr = l4_frame.start_address();
    let l4_virt_ptr = l4_phys_addr.as_u64() as *mut PageTable;
    core::ptr::write_bytes(l4_virt_ptr, 0, 1);

    let l4 = &mut *l4_virt_ptr;
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

    for addr in (0..0x80000000).step_by(2 * 1024 * 1024) {
        let l4_idx = ((addr >> 39) & 0x1FF) as usize;
        let l3_idx = ((addr >> 30) & 0x1FF) as usize;

        let l3_frame = frame_allocator.allocate_frame().expect("Failed to alloc L3");
        let l3_ptr = l3_frame.start_address().as_u64() as *mut PageTable;
        core::ptr::write_bytes(l3_ptr, 0, 1);
        l4[l4_idx].set_addr(l3_frame.start_address(), flags | PageTableFlags::PRESENT);

        let l2_frame = frame_allocator.allocate_frame().expect("Failed to alloc L2");
        let l2_ptr = l2_frame.start_address().as_u64() as *mut PageTable;
        core::ptr::write_bytes(l2_ptr, 0, 1);
        let l3 = &mut *l3_ptr;
        l3[l3_idx].set_addr(l2_frame.start_address(), flags | PageTableFlags::PRESENT);

        let l2 = &mut *l2_ptr;
        for i in 0..512 {
            let page_addr = addr + (i as u64) * (2 * 1024 * 1024);
            if page_addr >= 0x80000000 {
                break;
            }
            l2[i].set_addr(PhysAddr::new(page_addr), flags | PageTableFlags::HUGE_PAGE | PageTableFlags::PRESENT);
        }
    }

    let kernel_virt = physical_memory_offset.as_u64() + kernel_phys_start;
    let l4_idx = ((kernel_virt >> 39) & 0x1FF) as usize;
    let l3_idx = ((kernel_virt >> 30) & 0x1FF) as usize;
    let l2_idx = ((kernel_virt >> 21) & 0x1FF) as usize;
    let l1_idx = ((kernel_virt >> 12) & 0x1FF) as usize;

    let l3_frame = frame_allocator.allocate_frame().expect("Failed to alloc L3 for kernel");
    let l3_ptr = l3_frame.start_address().as_u64() as *mut PageTable;
    core::ptr::write_bytes(l3_ptr, 0, 1);
    l4[l4_idx].set_addr(l3_frame.start_address(), flags | PageTableFlags::PRESENT);

    let l2_frame = frame_allocator.allocate_frame().expect("Failed to alloc L2 for kernel");
    let l2_ptr = l2_frame.start_address().as_u64() as *mut PageTable;
    core::ptr::write_bytes(l2_ptr, 0, 1);
    let l3 = &mut *l3_ptr;
    l3[l3_idx].set_addr(l2_frame.start_address(), flags | PageTableFlags::PRESENT);

    let l1_frame = frame_allocator.allocate_frame().expect("Failed to alloc L1 for kernel");
    let l1_ptr = l1_frame.start_address().as_u64() as *mut PageTable;
    core::ptr::write_bytes(l1_ptr, 0, 1);
    let l2 = &mut *l2_ptr;
    l2[l2_idx].set_addr(l1_frame.start_address(), flags | PageTableFlags::PRESENT);

    let l1 = &mut *l1_ptr;
    l1[l1_idx].set_addr(PhysAddr::new(kernel_phys_start), flags);

    Cr3::write(
        PhysFrame::containing_address(l4_phys_addr),
        Cr3Flags::empty(),
    );

    x86_64::instructions::tlb::flush_all();

    let l4_higher_ptr = (l4_phys_addr.as_u64() + physical_memory_offset.as_u64()) as *mut PageTable;
    let mapper = OffsetPageTable::new(&mut *l4_higher_ptr, physical_memory_offset);

    PAGE_TABLE_INITIALIZED.store(true, Ordering::SeqCst);
    STORED_OFFSET = Some(physical_memory_offset);
    STORED_L4_PTR = Some(l4_higher_ptr);

    mapper
}

pub unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    let cr3 = Cr3::read().0.start_address();
    let phys = cr3.as_u64();
    let l4_ptr = (physical_memory_offset.as_u64() + phys) as *mut PageTable;
    &mut *l4_ptr
}