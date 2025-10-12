use core::alloc::{GlobalAlloc, Layout};
use core::ptr;
use petroleum::page_table::BootInfoFrameAllocator;

use spin::Mutex;
use x86_64::registers::control::Cr3Flags;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, Page, PageTableFlags as Flags, PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

// Helper function to iterate over memory descriptors with a specific type
fn for_each_memory_descriptor<F>(
    memory_map: &[petroleum::page_table::EfiMemoryDescriptor],
    types: &[petroleum::common::EfiMemoryType],
    mut f: F,
) where
    F: FnMut(&petroleum::page_table::EfiMemoryDescriptor),
{
    for desc in memory_map {
        if types.iter().any(|&t| desc.type_ == t) && desc.number_of_pages > 0 {
            f(desc);
        }
    }
}

pub const HEAP_SIZE: usize = 100 * 1024; // 100 KiB

fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

#[repr(C)]
struct ListNode {
    size: usize,
    next: *mut ListNode,
}

impl ListNode {
    fn new(size: usize) -> Self {
        ListNode {
            size,
            next: ptr::null_mut(),
        }
    }

    fn start_addr(&self) -> usize {
        self as *const Self as usize
    }

    fn end_addr(&self) -> usize {
        self.start_addr() + self.size
    }
}

pub struct Heap {
    head: *mut ListNode,
}

// SAFETY: This is a single-threaded kernel allocator
unsafe impl Send for Heap {}

impl Heap {
    pub const fn empty() -> Self {
        Heap {
            head: ptr::null_mut(),
        }
    }

    pub unsafe fn init(&mut self, heap_start: *mut u8, heap_size: usize) {
        // Initialize the free list with one big block
        let node = heap_start as *mut ListNode;
        unsafe {
            *node = ListNode::new(heap_size);
        }
        self.head = node;
    }

    fn alloc(&mut self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align();

        unsafe {
            let mut current = &mut self.head;
            while !(*current).is_null() {
                let node = &mut **current;
                let alloc_start = align_up(node.start_addr(), align);
                let alloc_end = alloc_start + size;
                let padding = alloc_start - node.start_addr();

                if alloc_end <= node.end_addr() {
                    // Found a suitable block
                    let remaining = node.end_addr() - alloc_end;
                    if remaining > core::mem::size_of::<ListNode>() {
                        // Split the block
                        let new_node = alloc_end as *mut ListNode;
                        unsafe {
                            *new_node = ListNode::new(remaining);
                            (*new_node).next = node.next;
                        }
                        node.next = new_node;
                    }
                    node.size = padding;
                    if node.size == 0 {
                        // Remove consumed node
                        *current = node.next;
                    }
                    return alloc_start as *mut u8;
                }
                current = &mut node.next;
            }
        }

        // No suitable block found
        ptr::null_mut()
    }

    unsafe fn insert_sorted(&mut self, new_node: *mut ListNode) {
        if self.head.is_null() || unsafe { (*new_node).start_addr() < (*self.head).start_addr() } {
            unsafe {
                (*new_node).next = self.head;
            }
            self.head = new_node;
            return;
        }

        let mut current = self.head;
        while !unsafe { (*current).next.is_null() }
            && unsafe { (*(*current).next).start_addr() < (*new_node).start_addr() }
        {
            current = unsafe { (*current).next };
        }

        unsafe {
            (*new_node).next = (*current).next;
            (*current).next = new_node;
        }
    }

    fn dealloc(&mut self, ptr: *mut u8, layout: Layout) {
        let size = layout.size();
        let _block_start = ptr as usize;

        unsafe {
            // Create a new free node
            let new_node = ptr as *mut ListNode;
            *new_node = ListNode::new(size);

            // Insert into the free list in sorted order by address
            self.insert_sorted(new_node);

            // Coalesce adjacent blocks
            self.coalesce();
        }
    }

    unsafe fn coalesce(&mut self) {
        if self.head.is_null() {
            return;
        }

        let mut current = self.head;
        while !unsafe { (*current).next.is_null() } {
            unsafe {
                let next = (*current).next;
                if (*current).end_addr() == (*next).start_addr() {
                    // Merge
                    (*current).size += (*next).size;
                    (*current).next = (*next).next;
                } else {
                    current = next;
                }
            }
        }
    }
}

pub struct Locked<A> {
    inner: spin::Mutex<A>,
}

impl<A> Locked<A> {
    pub const fn new(inner: A) -> Self {
        Locked {
            inner: spin::Mutex::new(inner),
        }
    }

    pub fn lock(&self) -> spin::MutexGuard<A> {
        self.inner.lock()
    }
}

unsafe impl GlobalAlloc for Locked<Heap> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.inner.lock().alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.inner.lock().dealloc(ptr, layout);
    }
}

#[global_allocator]
pub static ALLOCATOR: Locked<Heap> = Locked::new(Heap::empty());

pub static PHYSICAL_MEMORY_OFFSET: spin::Once<VirtAddr> = spin::Once::new();
pub static HIGHER_HALF_OFFSET: spin::Once<VirtAddr> = spin::Once::new();
pub(crate) static MAPPER: spin::Once<Mutex<OffsetPageTable<'static>>> = spin::Once::new();
pub(crate) static FRAME_ALLOCATOR: spin::Once<Mutex<BootInfoFrameAllocator<'static>>> =
    spin::Once::new();
static MEMORY_MAP: spin::Once<&'static [petroleum::page_table::EfiMemoryDescriptor]> =
    spin::Once::new();

pub fn init(heap_start: VirtAddr, heap_size: usize) {
    // Map heap pages to virtual addresses in the current page table
    let mut mapper = MAPPER.get().unwrap().lock();
    let mut frame_allocator = FRAME_ALLOCATOR.get().unwrap().lock();
    let physical_memory_offset = *PHYSICAL_MEMORY_OFFSET.get().unwrap();

    let start_page = Page::<Size4KiB>::containing_address(heap_start);
    let end_address = heap_start + heap_size as u64;
    let end_page = Page::<Size4KiB>::containing_address(end_address - 1);

    let mut current_page = start_page;
    while current_page <= end_page {
        let page_start_virt = current_page.start_address();
        let page_start_phys = PhysAddr::new(page_start_virt.as_u64() - physical_memory_offset.as_u64());
        let frame = PhysFrame::<Size4KiB>::containing_address(page_start_phys);

        unsafe {
            mapper
                .map_to(current_page, frame, Flags::PRESENT | Flags::WRITABLE, &mut *frame_allocator)
                .unwrap()
                .flush();
        }

        current_page = current_page + 1;
    }

    drop(frame_allocator);
    drop(mapper);

    unsafe {
        let heap_start_ptr = heap_start.as_mut_ptr::<u8>();
        ALLOCATOR.lock().init(heap_start_ptr, heap_size);
    }
}

pub fn init_page_table(physical_memory_offset: VirtAddr) {
    PHYSICAL_MEMORY_OFFSET.call_once(|| physical_memory_offset);
    let mapper = unsafe { petroleum::page_table::init(physical_memory_offset) };
    MAPPER.call_once(|| Mutex::new(mapper));
}

pub fn init_frame_allocator(memory_map: &'static [petroleum::page_table::EfiMemoryDescriptor]) {
    let allocator = unsafe { BootInfoFrameAllocator::init(memory_map) };
    FRAME_ALLOCATOR.call_once(|| Mutex::new(allocator));
    MEMORY_MAP.call_once(|| memory_map);
}

/// Helper function to map a contiguous physical memory range to virtual memory
pub(crate) unsafe fn map_physical_range(
    mapper: &mut OffsetPageTable,
    start_phys: PhysAddr,
    end_phys: PhysAddr,
    start_virt: VirtAddr,
    flags: Flags,
    frame_allocator: &mut BootInfoFrameAllocator,
) {
    let mut current_phys = start_phys;
    while current_phys < end_phys {
        let virt_addr = start_virt + (current_phys - start_phys);
        let page = Page::<Size4KiB>::containing_address(virt_addr);
        let frame = PhysFrame::<Size4KiB>::containing_address(current_phys);

        unsafe {
            mapper
                .map_to(page, frame, flags, frame_allocator)
                .expect("Failed to map page")
                .flush();
        }

        current_phys += 4096u64;
    }
}

pub fn reinit_page_table(physical_memory_offset: VirtAddr, kernel_phys_start: PhysAddr, framebuffer_addr: Option<u64>, framebuffer_size: Option<u64>) {
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::PageTable;

    petroleum::serial::serial_log(format_args!("reinit_page_table: Starting with offset 0x{:x}, kernel_start 0x{:x}\n", physical_memory_offset.as_u64(), kernel_phys_start.as_u64()));

    let mut frame_allocator = FRAME_ALLOCATOR.get().unwrap().lock();
    let memory_map = *MEMORY_MAP.get().unwrap();

    // Allocate a new level 4 page table
    let level_4_frame = frame_allocator
        .allocate_frame()
        .expect("Failed to allocate level 4 frame");
    petroleum::serial::serial_log(format_args!("reinit_page_table: Allocated L4 frame at 0x{:x}\n", level_4_frame.start_address().as_u64()));

    // Temporarily map the new level 4 table to an unused virtual address for initialization
    let temp_virt_page = Page::<Size4KiB>::containing_address(VirtAddr::new(0xFFFF_FF00_0000_F000)); // Example high canonical address
    {
        let mut current_mapper = MAPPER.get().unwrap().lock();
        unsafe {
            current_mapper.map_to(temp_virt_page, level_4_frame, Flags::PRESENT | Flags::WRITABLE, &mut *frame_allocator).expect("Failed to map temp");
        }
    }
    petroleum::serial::serial_log(format_args!("reinit_page_table: Temp mapped L4 table\n"));

    let level_4_table: &mut PageTable = unsafe { &mut *temp_virt_page.start_address().as_mut_ptr() };
    level_4_table.zero();
    petroleum::serial::serial_log(format_args!("reinit_page_table: Zeroed L4 table\n"));

    // Create a mapper for the new page table
    let mut new_mapper = unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) };
    petroleum::serial::serial_log(format_args!("reinit_page_table: Created new mapper\n"));

    // Identity map conventional memory to allow page table allocations during setup
    let mut mapped_conventional = 0;
    for desc in memory_map {
        if desc.type_ == petroleum::common::EfiMemoryType::EfiConventionalMemory {
            let start_phys = PhysAddr::new(desc.physical_start);
            let end_phys = start_phys + (desc.number_of_pages * 4096);
            let start_virt = VirtAddr::new(desc.physical_start);

            unsafe {
                map_physical_range(
                    &mut new_mapper,
                    start_phys,
                    end_phys,
                    start_virt,
                    Flags::PRESENT | Flags::WRITABLE,
                    &mut frame_allocator,
                );
            }
            mapped_conventional += desc.number_of_pages;
        }
    }
    petroleum::serial::serial_log(format_args!("reinit_page_table: Identity mapped {} conventional pages\n", mapped_conventional));

    // Calculate higher half offset for kernel mapping (use canonical higher-half address)
    // In x86_64, higher half starts at 0xFFFF800000000000
    let higher_half_offset = VirtAddr::new(0xFFFF_8000_0000_0000);
    petroleum::serial::serial_log(format_args!("reinit_page_table: Using higher half offset 0x{:x}\n", higher_half_offset.as_u64()));

    // Map all memory regions appropriately
    let mut mapped_kernel = 0;
    for desc in memory_map {
        if desc.number_of_pages == 0 {
            continue;
        }

        let start_phys = PhysAddr::new(desc.physical_start);
        let end_phys = start_phys + (desc.number_of_pages * 4096);

        // Choose appropriate virtual address based on memory region
        let (start_virt, flags) = match desc.type_ {
            // Kernel code and data - map to higher half
            petroleum::common::EfiMemoryType::EfiLoaderCode | petroleum::common::EfiMemoryType::EfiLoaderData => {
                let virt_start = higher_half_offset + desc.physical_start;
                mapped_kernel += desc.number_of_pages;
                (virt_start, Flags::PRESENT | Flags::WRITABLE)
            },
            // Conventional memory - keep identity mapping for boot-time allocations
            petroleum::common::EfiMemoryType::EfiConventionalMemory => {
                let virt_start = VirtAddr::new(desc.physical_start);
                (virt_start, Flags::PRESENT | Flags::WRITABLE)
            },
            // Other memory types - identity mapping
            _ => {
                let virt_start = VirtAddr::new(desc.physical_start);
                (virt_start, Flags::PRESENT | Flags::WRITABLE)
            }
        };

        unsafe {
            map_physical_range(
                &mut new_mapper,
                start_phys,
                end_phys,
                start_virt,
                flags,
                &mut frame_allocator,
            );
        }
    }
    petroleum::serial::serial_log(format_args!("reinit_page_table: Mapped {} kernel pages to higher half\n", mapped_kernel));

    // Ensure VGA buffer region is mapped (for compatibility)
    let vga_start = PhysAddr::new(0xA0000);
    let vga_end = PhysAddr::new(0xC0000);
    let vga_virt = VirtAddr::new(0xA0000);
    unsafe {
        map_physical_range(
            &mut new_mapper,
            vga_start,
            vga_end,
            vga_virt,
            Flags::PRESENT | Flags::WRITABLE,
            &mut frame_allocator,
        );
    }
    petroleum::serial::serial_log(format_args!("reinit_page_table: Mapped VGA buffer\n"));

    // Map framebuffer if provided
    if let Some(fb_addr) = framebuffer_addr {
        petroleum::serial::serial_log(format_args!("reinit_page_table: Mapping framebuffer at 0x{:x}\n", fb_addr));
        let fb_start = PhysAddr::new(fb_addr);
        // Calculate actual framebuffer size or use provided size, fallback to 4MB
        let fb_size = framebuffer_size.unwrap_or(0x400000);
        let fb_end = fb_start + fb_size;
        let fb_virt = VirtAddr::new(fb_addr); // Identity mapping for framebuffer
        unsafe {
            map_physical_range(
                &mut new_mapper,
                fb_start,
                fb_end,
                fb_virt,
                Flags::PRESENT | Flags::WRITABLE,
                &mut frame_allocator,
            );
        }
        petroleum::serial::serial_log(format_args!("reinit_page_table: Mapped framebuffer of size 0x{:x}\n", fb_size));
    } else {
        petroleum::serial::serial_log(format_args!("reinit_page_table: No framebuffer to map\n"));
    }

    // Set HIGHER_HALF_OFFSET for kernel use
    HIGHER_HALF_OFFSET.call_once(|| higher_half_offset);
    petroleum::serial::serial_log(format_args!("reinit_page_table: Set HIGHER_HALF_OFFSET\n"));

    // Map the level4 table itself to higher half virtual address for access after CR3 switch
    let level4_phys = level_4_frame.start_address();
    let level4_virt_addr_u64 = higher_half_offset.as_u64() + level4_phys.as_u64();
    let level4_virt_page = Page::<Size4KiB>::containing_address(VirtAddr::new(level4_virt_addr_u64));
    unsafe {
        new_mapper.map_to(level4_virt_page, level_4_frame, Flags::PRESENT | Flags::WRITABLE, &mut *frame_allocator).expect("Failed to map level4 in higher half").flush();
    }
    petroleum::serial::serial_log(format_args!("reinit_page_table: Level4 table mapped to higher half\n"));

    // Unmap temporary mapping
    {
        let mut current_mapper = MAPPER.get().unwrap().lock();
        current_mapper.unmap(temp_virt_page).expect("Failed to unmap temp").1.flush();
    }
    petroleum::serial::serial_log(format_args!("reinit_page_table: Unmapped temp\n"));

    petroleum::serial::serial_log(format_args!("reinit_page_table: About to write CR3\n"));
    // Switch to new page table
    unsafe { Cr3::write(level_4_frame, Cr3Flags::empty()) };
    petroleum::serial::serial_log(format_args!("reinit_page_table: CR3 written, switched to new page table\n"));

    // Reinitialize global mapper with new page table
    let new_physical_memory_offset = if physical_memory_offset.as_u64() == 0 {
        // If original offset was 0 (identity), update to use higher-half for kernel access
        higher_half_offset
    } else {
        physical_memory_offset
    };
    petroleum::serial::serial_log(format_args!("reinit_page_table: Reinitializing mapper with offset 0x{:x}\n", new_physical_memory_offset.as_u64()));

    let mapper = unsafe { petroleum::page_table::init(new_physical_memory_offset) };
    *MAPPER.get().unwrap().lock() = mapper;
    petroleum::serial::serial_log(format_args!("reinit_page_table: Global mapper reinitialized\n"));

    petroleum::serial::serial_log(format_args!("reinit_page_table: Completed successfully\n"));
}

// Allocate heap from memory map (find virtual address from physical)
pub fn allocate_heap_from_map(phys_start: PhysAddr, _size: usize) -> VirtAddr {
    // Use higher-half virtual address for heap - physical address directly mapped to higher half
    // Higher half mapping: physical + 0xFFFF_8000_0000_0000 = higher half virtual
    VirtAddr::new(0xFFFF_8000_0000_0000 + phys_start.as_u64())
}
