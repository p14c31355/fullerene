//! Page table initialization and kernel jump logic.

use crate::page_table::allocator::bitmap::BitmapFrameAllocator;
use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::Cr3,
    structures::paging::{FrameAllocator, OffsetPageTable, PageTable, PageTableFlags, Size4KiB},
};

static PAGE_TABLE_INITIALIZED: AtomicBool = AtomicBool::new(false);
static mut STORED_OFFSET: Option<VirtAddr> = None;
static mut STORED_L4_PTR: Option<*mut PageTable> = None;

const PAGE_SIZE_4K: u64 = 4096;
const PAGE_SIZE_2M: u64 = 2 * 1024 * 1024;
const ENTRIES_PER_TABLE: u64 = 512;
const IDENTITY_MAP_2MB_PAGES: u64 = 32768; // 64GB / 2MB

#[derive(Clone, Copy)]
struct PageTableIndices {
    l4: usize,
    l3: usize,
    l2: usize,
    l1: usize,
}

impl PageTableIndices {
    fn new(virt: VirtAddr) -> Self {
        Self {
            l4: ((virt.as_u64() >> 39) & 0x1FF) as usize,
            l3: ((virt.as_u64() >> 30) & 0x1FF) as usize,
            l2: ((virt.as_u64() >> 21) & 0x1FF) as usize,
            l1: ((virt.as_u64() >> 12) & 0x1FF) as usize,
        }
    }
}

/// Stateful low-level page-table editor for kernel bootstrap memory work.
///
/// This intentionally gathers the raw pointer arithmetic, frame allocation,
/// table zeroing, huge-page splitting, and range mapping in one place.  Callers
/// should prefer this over open-coding page-table walks.
pub struct KernelMemoryOperations<'a> {
    l4: *mut PageTable,
    frame_allocator: *mut BitmapFrameAllocator,
    page_table_access_offset: VirtAddr,
    default_flags: PageTableFlags,
    _marker: core::marker::PhantomData<&'a mut (PageTable, BitmapFrameAllocator)>,
}

impl<'a> KernelMemoryOperations<'a> {
    /// Create a page-table editor.
    ///
    /// # Safety
    /// `l4` must point to a valid mutable L4 table, and physical page-table
    /// addresses must be accessible through `page_table_access_offset`.
    pub unsafe fn new(
        l4: &'a mut PageTable,
        frame_allocator: &'a mut BitmapFrameAllocator,
        page_table_access_offset: VirtAddr,
        default_flags: PageTableFlags,
    ) -> Self {
        Self {
            l4: l4 as *mut PageTable,
            frame_allocator: frame_allocator as *mut BitmapFrameAllocator,
            page_table_access_offset,
            default_flags,
            _marker: core::marker::PhantomData,
        }
    }

    fn table_ptr(&self, phys: PhysAddr) -> *mut PageTable {
        (phys.as_u64() + self.page_table_access_offset.as_u64()) as *mut PageTable
    }

    unsafe fn zero_table_at_offset(phys: PhysAddr, page_table_access_offset: VirtAddr) {
        unsafe {
            let ptr = (phys.as_u64() + page_table_access_offset.as_u64()) as *mut u8;
            core::ptr::write_bytes(ptr, 0, PAGE_SIZE_4K as usize);
        }
    }

    unsafe fn allocate_zeroed_table_from(
        frame_allocator: *mut BitmapFrameAllocator,
        page_table_access_offset: VirtAddr,
        error: &'static str,
    ) -> Result<PhysAddr, &'static str> {
        let frame = unsafe { &mut *frame_allocator }
            .allocate_frame()
            .ok_or(error)?;
        let addr = frame.start_address();
        unsafe { Self::zero_table_at_offset(addr, page_table_access_offset) };
        Ok(addr)
    }

    /// Zero the root L4 table before building a fresh kernel address space.
    pub unsafe fn zero_root(&mut self) {
        unsafe {
            core::ptr::write_bytes(self.l4 as *mut u8, 0, PAGE_SIZE_4K as usize);
        }
    }

    /// Map one 4 KiB page, splitting existing 1 GiB/2 MiB huge pages as needed.
    pub unsafe fn map_page_4k(
        &mut self,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageTableFlags,
    ) -> Result<(), &'static str> {
        unsafe {
            let indices = PageTableIndices::new(virt);
            let offset = self.page_table_access_offset.as_u64();
            let frame_allocator = self.frame_allocator;
            let page_table_access_offset = self.page_table_access_offset;

            let l4 = &mut *self.l4;
            let l3 = if l4[indices.l4].is_unused() {
                let addr = Self::allocate_zeroed_table_from(
                    frame_allocator,
                    page_table_access_offset,
                    "4k: alloc L3 failed",
                )?;
                l4[indices.l4].set_addr(addr, flags | PageTableFlags::PRESENT);
                &mut *((l4[indices.l4].addr().as_u64() + offset) as *mut PageTable)
            } else {
                &mut *((l4[indices.l4].addr().as_u64() + offset) as *mut PageTable)
            };

            if l3[indices.l3].flags().contains(PageTableFlags::HUGE_PAGE) {
                let huge_phys_base = l3[indices.l3].addr().as_u64();
                let orig_flags = l3[indices.l3].flags();
                let l2_phys = Self::allocate_zeroed_table_from(
                    frame_allocator,
                    page_table_access_offset,
                    "4k: alloc L2 for 1GB split failed",
                )?;
                let l2_ref = &mut *self.table_ptr(l2_phys);
                for j in 0..ENTRIES_PER_TABLE {
                    l2_ref[j as usize].set_addr(
                        PhysAddr::new(huge_phys_base + j * PAGE_SIZE_2M),
                        orig_flags | PageTableFlags::PRESENT | PageTableFlags::HUGE_PAGE,
                    );
                }
                let mut new_flags = orig_flags;
                new_flags.remove(PageTableFlags::HUGE_PAGE);
                l3[indices.l3].set_addr(l2_phys, new_flags | PageTableFlags::PRESENT);
            }

            let l2 = if l3[indices.l3].is_unused() {
                let addr = Self::allocate_zeroed_table_from(
                    frame_allocator,
                    page_table_access_offset,
                    "4k: alloc L2 failed",
                )?;
                l3[indices.l3].set_addr(addr, flags | PageTableFlags::PRESENT);
                &mut *((l3[indices.l3].addr().as_u64() + offset) as *mut PageTable)
            } else {
                &mut *((l3[indices.l3].addr().as_u64() + offset) as *mut PageTable)
            };

            if l2[indices.l2].is_unused() {
                let l1_phys = Self::allocate_zeroed_table_from(
                    frame_allocator,
                    page_table_access_offset,
                    "4k: alloc L1 failed",
                )?;
                l2[indices.l2].set_addr(l1_phys, flags | PageTableFlags::PRESENT);
            } else if l2[indices.l2].flags().contains(PageTableFlags::HUGE_PAGE) {
                let huge_page_phys_base = l2[indices.l2].addr().as_u64();
                let l1_phys = Self::allocate_zeroed_table_from(
                    frame_allocator,
                    page_table_access_offset,
                    "4k: split L1 failed",
                )?;
                let l1_ref = &mut *self.table_ptr(l1_phys);
                for j in 0..ENTRIES_PER_TABLE {
                    l1_ref[j as usize]
                        .set_addr(PhysAddr::new(huge_page_phys_base + j * PAGE_SIZE_4K), flags);
                }
                l2[indices.l2].set_addr(l1_phys, flags | PageTableFlags::PRESENT);
            }

            let l1 = &mut *((l2[indices.l2].addr().as_u64() + offset) as *mut PageTable);
            l1[indices.l1].set_addr(phys, flags);
            Ok(())
        }
    }

    /// Map a range using 4 KiB pages.
    pub unsafe fn map_range_4k(
        &mut self,
        virt_start: VirtAddr,
        phys_start: PhysAddr,
        page_count: u64,
        flags: PageTableFlags,
    ) -> Result<(), &'static str> {
        for i in 0..page_count {
            let virt = VirtAddr::new(virt_start.as_u64() + i * PAGE_SIZE_4K);
            let phys = PhysAddr::new(phys_start.as_u64() + i * PAGE_SIZE_4K);
            unsafe { self.map_page_4k(virt, phys, flags)? };
        }
        Ok(())
    }

    /// Map a range using 2 MiB huge pages.
    pub unsafe fn map_range_2mb_huge(
        &mut self,
        virt_start: VirtAddr,
        phys_start: PhysAddr,
        page_count: u64,
        flags: PageTableFlags,
    ) -> Result<(), &'static str> {
        unsafe {
            let flags_2mb = flags | PageTableFlags::HUGE_PAGE;
            for i in 0..page_count {
                let virt = VirtAddr::new(virt_start.as_u64() + i * PAGE_SIZE_2M);
                let phys = PhysAddr::new(phys_start.as_u64() + i * PAGE_SIZE_2M);
                let indices = PageTableIndices::new(virt);
                let offset = self.page_table_access_offset.as_u64();
                let frame_allocator = self.frame_allocator;
                let page_table_access_offset = self.page_table_access_offset;

                let l4 = &mut *self.l4;
                if l4[indices.l4].is_unused() {
                    let addr = Self::allocate_zeroed_table_from(
                        frame_allocator,
                        page_table_access_offset,
                        "huge: alloc L3 failed",
                    )?;
                    l4[indices.l4].set_addr(addr, flags | PageTableFlags::PRESENT);
                }

                let l3 = &mut *((l4[indices.l4].addr().as_u64() + offset) as *mut PageTable);

                if l3[indices.l3].is_unused() {
                    let addr = Self::allocate_zeroed_table_from(
                        frame_allocator,
                        page_table_access_offset,
                        "huge: alloc L2 failed",
                    )?;
                    l3[indices.l3].set_addr(addr, flags | PageTableFlags::PRESENT);
                }

                let l2 = &mut *((l3[indices.l3].addr().as_u64() + offset) as *mut PageTable);
                l2[indices.l2].set_addr(phys, flags_2mb | PageTableFlags::PRESENT);
            }
            Ok(())
        }
    }

    /// Map the initial identity and higher-half 64 GiB windows used by bootstrap.
    pub unsafe fn map_bootstrap_windows(
        &mut self,
        physical_memory_offset: VirtAddr,
    ) -> Result<(), &'static str> {
        unsafe {
            self.map_range_2mb_huge(
                VirtAddr::new(0),
                PhysAddr::new(0),
                IDENTITY_MAP_2MB_PAGES,
                self.default_flags,
            )?;
            self.map_range_2mb_huge(
                physical_memory_offset,
                PhysAddr::new(0),
                IDENTITY_MAP_2MB_PAGES,
                self.default_flags,
            )?;
            Ok(())
        }
    }
}

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
    unsafe {
        KernelMemoryOperations::new(l4, frame_allocator, phys_offset, flags)
            .map_page_4k(virt, phys, flags)
    }
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
    unsafe {
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
        let this_func_addr = init_and_jump as *const () as usize;
        crate::serial::_print(format_args!("IAJ: this_func_phys={:#x}\n", this_func_addr));

        // Based on the success pattern, reset the segment registers to clean the execution environment.
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

        let current_rsp: usize;
        core::arch::asm!("mov {}, rsp", out(reg) current_rsp);
        crate::serial::_print(format_args!(
            "IAJ: args_ptr={:#x}, l4_phys={:#x}, stack_top={:#x}, current_rsp={:#x}\n",
            args_ptr as usize, l4_phys_addr, stack_top, current_rsp
        ));

        crate::serial::_print(format_args!("IAJ: Initializing L4 table...\n"));

        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

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
        let l4 = &mut *l4_ptr;
        let mut memory_ops =
            KernelMemoryOperations::new(l4, frame_allocator, VirtAddr::new(0), flags);
        memory_ops.zero_root();

        crate::serial::_print(format_args!("IAJ: Using pre-allocated L4\n"));

        // STEP 1: Identity-map the entire 64GB physical address space using 2MB huge pages.
        // This must be done FIRST so that 4KB mappings can split specific huge pages later.
        // 64GB = 32768 × 2MB pages. This ensures VirtIO DMA buffers (which can be allocated
        // anywhere by the frame allocator) are accessible.
        crate::serial::_print(format_args!("IAJ: Mapping identity huge pages (64GB)...\n"));
        memory_ops
            .map_range_2mb_huge(
                VirtAddr::new(0),
                PhysAddr::new(0),
                IDENTITY_MAP_2MB_PAGES,
                flags,
            )
            .expect("full 64GB huge page identity map failed");
        crate::serial::_print(format_args!("IAJ: Huge page identity mapped\n"));

        // STEP 1b: Map the entire 0-64GB physical range to the higher half using 2MB huge pages.
        // This MUST be done BEFORE any 4KB splits so that the 4KB functions can properly
        // split the higher-half huge pages.
        // This is CRITICAL: OffsetPageTable (used by map_to etc.) accesses page table
        // structures through phys_offset + table_phys_addr, and page tables can be
        // allocated at any physical address within 0-64GB by the frame allocator.
        crate::serial::_print(format_args!(
            "IAJ: Mapping full 64GB to higher half (huge pages)...\n"
        ));
        memory_ops
            .map_range_2mb_huge(
                physical_memory_offset,
                PhysAddr::new(0),
                IDENTITY_MAP_2MB_PAGES,
                flags,
            )
            .expect("full 64GB huge page higher-half map failed");
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
        memory_ops
            .map_range_4k(
                kernel_virt,
                PhysAddr::new(kernel_phys_start),
                kernel_pages,
                flags,
            )
            .expect("kernel higher map");

        // identity kernel mapping (splits identity huge pages)
        memory_ops
            .map_range_4k(
                VirtAddr::new(kernel_phys_start),
                PhysAddr::new(kernel_phys_start),
                kernel_pages,
                flags,
            )
            .expect("kernel identity");
        crate::serial::_print(format_args!("IAJ: Kernel mapped\n"));

        // === Stack mapping (identity + higher-half) ===
        let stack_phys_page = (stack_top & 0x00000000_FFFFF000) as u64;
        crate::serial::_print(format_args!(
            "IAJ: Mapping stack... stack_top={:#x}, stack_phys_page={:#x}\n",
            stack_top, stack_phys_page
        ));
        let stack_pages = 256u64; // 1MB — kernel init (memory manager, MMIO, APIC, graphics) exceeds 32 KB
        let stack_phys_base = stack_phys_page - stack_pages * 4096 + 4096;

        // identity
        memory_ops
            .map_range_4k(
                VirtAddr::new(stack_phys_base),
                PhysAddr::new(stack_phys_base),
                stack_pages,
                flags,
            )
            .expect("stack identity");
        // higher-half
        memory_ops
            .map_range_4k(
                VirtAddr::new(physical_memory_offset.as_u64() + stack_phys_base),
                PhysAddr::new(stack_phys_base),
                stack_pages,
                flags,
            )
            .expect("stack higher");
        crate::serial::_print(format_args!("IAJ: Stack mapped\n"));

        // Args (identity only - higher-half covered by huge page splits)
        let args_pages = 1u64;
        memory_ops
            .map_range_4k(VirtAddr::new(arg1), PhysAddr::new(arg1), args_pages, flags)
            .expect("args map");

        // Memory map (identity + higher-half)
        let map_pages = (map_size + 4095) / 4096;
        memory_ops
            .map_range_4k(
                VirtAddr::new(map_phys_addr),
                PhysAddr::new(map_phys_addr),
                map_pages,
                flags,
            )
            .expect("map identity");
        let map_virt_higher = VirtAddr::new(physical_memory_offset.as_u64() + map_phys_addr);
        memory_ops
            .map_range_4k(
                map_virt_higher,
                PhysAddr::new(map_phys_addr),
                map_pages,
                flags,
            )
            .expect("map higher");

        // Store state BEFORE switching CR3, since after the switch we can't safely reference
        // Rust statics (they may be in unmapped higher-half addresses)
        PAGE_TABLE_INITIALIZED.store(true, Ordering::SeqCst);
        STORED_OFFSET = Some(physical_memory_offset);
        STORED_L4_PTR = Some(l4_ptr);

        // VGA debug: about to switch CR3
        crate::vga_debug::vga_puts(22, 0, b"IAJ:sw cr3");
        crate::serial::_print(format_args!("IAJ: mappings done, switching CR3...\n"));
        x86_64::registers::control::Cr3::write(
            x86_64::structures::paging::PhysFrame::containing_address(PhysAddr::new(l4_phys)),
            x86_64::registers::control::Cr3Flags::empty(),
        );
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
}

/// Legacy init function - kept for compatibility.
///
/// This always reuses the currently active page table (CR3) set up by init_and_jump
/// from the bootloader, creating an OffsetPageTable mapper for it.
pub unsafe fn init<A: FrameAllocator<Size4KiB>, F>(
    physical_memory_offset: VirtAddr,
    _frame_allocator: &mut A,
    _kernel_phys_start: u64,
    _early_mappings: Option<F>,
) -> OffsetPageTable<'static>
where
    F: FnOnce(&mut OffsetPageTable, &mut A),
{
    unsafe {
        crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [init] entered\n");

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
}

pub unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    unsafe {
        let cr3 = Cr3::read().0.start_address();
        let phys = cr3.as_u64();
        let l4_ptr = (physical_memory_offset.as_u64() + phys) as *mut PageTable;
        &mut *l4_ptr
    }
}
