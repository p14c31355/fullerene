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

pub fn reinit_page_table(physical_memory_offset: VirtAddr, kernel_phys_start: PhysAddr, framebuffer_addr: Option<u64>) {
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::PageTable;

    petroleum::serial::serial_log(format_args!("reinit_page_table: Starting with offset 0x{:x}\n", physical_memory_offset.as_u64()));

    let mut frame_allocator = FRAME_ALLOCATOR.get().unwrap().lock();
    let memory_map = *MEMORY_MAP.get().unwrap();

    petroleum::serial::serial_log(format_args!("reinit_page_table: Allocated new L4 frame\n"));

    // Allocate a new level 4 page table
    let level_4_frame = frame_allocator
        .allocate_frame()
        .expect("Failed to allocate level 4 frame");

    // Temporarily map the new level 4 table to an unused virtual address
    let temp_virt_page = Page::<Size4KiB>::containing_address(VirtAddr::new(0xFFFF_F000)); // Use a low virtual address for temporary mapping
    {
        let mut current_mapper = MAPPER.get().unwrap().lock();
        unsafe {
            current_mapper.map_to(temp_virt_page, level_4_frame, Flags::PRESENT | Flags::WRITABLE, &mut *frame_allocator).expect("Failed to map temp");
        }
    }

    let level_4_table: &mut PageTable = unsafe { &mut *temp_virt_page.start_address().as_mut_ptr() };

    // Zero the table
    level_4_table.zero();

    // Create a mapper for the new page table using the same offset as the original mapping
    let mut new_mapper = unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) };

    // Identity map conventional memory in the new page table to allow page table allocations
    for desc in memory_map {
        if desc.type_ == petroleum::common::EfiMemoryType::EfiConventionalMemory {
            let start_phys = PhysAddr::new(desc.physical_start);
            let end_phys = start_phys + (desc.number_of_pages * 4096);
            let start_virt = VirtAddr::new(desc.physical_start); // identity mapping

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
        }
    }

    petroleum::serial::serial_log(format_args!("reinit_page_table: New mapper created\n"));

    // For UEFI with identity mapping (offset = 0), use identity mapping consistently
    // Do not use UEFI virtual addresses as they may not be properly set
    petroleum::serial::serial_log(format_args!("reinit_page_table: Mapping memory regions with identity mapping\n"));

    for desc in memory_map {
        let start_phys = PhysAddr::new(desc.physical_start);
        let end_phys = start_phys + (desc.number_of_pages * 4096);
        let start_virt = VirtAddr::new(desc.physical_start); // Identity mapping: virt = phys

        // Map all memory types using identity mapping - VESA/VGA buffers may be in unmapped regions
        let should_map = desc.number_of_pages > 0;

        if should_map {
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
        }
    }

    // Ensure VGA buffer region is mapped (0xA0000 - 0xC0000)
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

    // Unmap the temporary mapping
    {
        let mut current_mapper = MAPPER.get().unwrap().lock();
            current_mapper.unmap(temp_virt_page).expect("Failed to unmap temp").1.flush();
    }

    // Set the new CR3
    unsafe { Cr3::write(level_4_frame, Cr3Flags::empty()) };

    petroleum::serial::serial_log(format_args!("reinit_page_table: CR3 updated\n"));

    // Reinitialize the mapper with new CR3 using the same physical_memory_offset
    let mapper = unsafe { petroleum::page_table::init(physical_memory_offset) };
    *MAPPER.get().unwrap().lock() = mapper;

    petroleum::serial::serial_log(format_args!("reinit_page_table: Completed successfully\n"));
}

// Allocate heap from memory map (find virtual address from physical)
pub fn allocate_heap_from_map(phys_start: PhysAddr, _size: usize) -> VirtAddr {
    // Use higher-half virtual address for heap
    if let Some(offset) = HIGHER_HALF_OFFSET.get() {
        VirtAddr::new(offset.as_u64() + phys_start.as_u64())
    } else {
        panic!("Higher half offset not initialized");
    }
}
