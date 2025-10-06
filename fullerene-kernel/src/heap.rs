use core::alloc::{GlobalAlloc, Layout};
use core::ptr;
use x86_64::{PhysAddr, VirtAddr};
use spin::Mutex;
use petroleum::page_table::{BootInfoFrameAllocator, EfiMemoryDescriptor};
use x86_64::structures::paging::{FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PhysFrame, PageTableFlags as Flags, Size4KiB};
use x86_64::registers::control::Cr3Flags;

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
        *node = ListNode::new(heap_size);
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

                if alloc_end <= node.end_addr() {
                    // Found a suitable block
                    let remaining = node.end_addr() - alloc_end;
                    if remaining > core::mem::size_of::<ListNode>() {
                        // Split the block
                        let new_node = alloc_end as *mut ListNode;
                        *new_node = ListNode::new(remaining);
                        (*new_node).next = node.next;
                        node.next = new_node;
                    }
                    node.size = alloc_start - node.start_addr();
                    return alloc_start as *mut u8;
                }
                current = &mut node.next;
            }
        }

        // No suitable block found
        ptr::null_mut()
    }

    fn dealloc(&mut self, ptr: *mut u8, layout: Layout) {
        let size = layout.size();
        let block_start = ptr as usize;

        unsafe {
            // Create a new free node
            let new_node = ptr as *mut ListNode;
            *new_node = ListNode::new(size);

            // Insert into the free list (simple insertion at head for now)
            (*new_node).next = self.head;
            self.head = new_node;

            // Coalesce adjacent blocks
            self.coalesce();
        }
    }

    unsafe fn coalesce(&mut self) {
        if self.head.is_null() {
            return;
        }

        // Simple bubble sort by address and merge
        let mut changed = true;
        while changed {
            changed = false;
            let mut current = self.head;
            while !(*current).next.is_null() {
                let next = (*current).next;
                if (*current).end_addr() == (*next).start_addr() {
                    // Merge
                    (*current).size += (*next).size;
                    (*current).next = (*next).next;
                    changed = true;
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

static MAPPER: spin::Once<Mutex<OffsetPageTable<'static>>> = spin::Once::new();
static FRAME_ALLOCATOR: spin::Once<Mutex<BootInfoFrameAllocator<'static>>> = spin::Once::new();
static MEMORY_MAP: spin::Once<&'static [petroleum::page_table::EfiMemoryDescriptor]> = spin::Once::new();

pub fn init(heap_start: VirtAddr, heap_size: usize) {
    unsafe {
        let heap_start = heap_start.as_mut_ptr::<u8>();
        ALLOCATOR.lock().init(heap_start, heap_size);
    }
}

pub fn init_page_table(physical_memory_offset: VirtAddr) {
    let mapper = unsafe { petroleum::page_table::init(physical_memory_offset) };
    MAPPER.call_once(|| Mutex::new(mapper));
}

pub fn init_frame_allocator(memory_map: &'static [petroleum::page_table::EfiMemoryDescriptor]) {
    let allocator = unsafe { BootInfoFrameAllocator::init(memory_map) };
    FRAME_ALLOCATOR.call_once(|| Mutex::new(allocator));
    MEMORY_MAP.call_once(|| memory_map);
}

pub fn reinit_page_table() {
    use x86_64::structures::paging::{PageTable, PageTableFlags as Flags};
    use x86_64::registers::control::Cr3;

    let mut frame_allocator = FRAME_ALLOCATOR.get().unwrap().lock();

    // Allocate a new level 4 page table
    let level_4_frame = frame_allocator.allocate_frame().expect("Failed to allocate level 4 frame");
    let level_4_table = unsafe { &mut *(level_4_frame.start_address().as_u64() as *mut PageTable) };

    // Zero the table
    level_4_table.zero();

    // Set up identity mapping for the first 4 GiB
    for i in 0..512 {
        let page_table_3_frame = frame_allocator.allocate_frame().expect("Failed to allocate level 3 frame");
        let page_table_3 = unsafe { &mut *(page_table_3_frame.start_address().as_u64() as *mut PageTable) };
        page_table_3.zero();

        for j in 0..512 {
            let page_table_2_frame = frame_allocator.allocate_frame().expect("Failed to allocate level 2 frame");
            let page_table_2 = unsafe { &mut *(page_table_2_frame.start_address().as_u64() as *mut PageTable) };
            page_table_2.zero();

            for k in 0..512 {
                let page_table_1_frame = frame_allocator.allocate_frame().expect("Failed to allocate level 1 frame");
                let page_table_1 = unsafe { &mut *(page_table_1_frame.start_address().as_u64() as *mut PageTable) };
                page_table_1.zero();

                // Identity map 2 MiB pages
                for l in 0..512 {
                    let addr = ((i as u64) << 39) | ((j as u64) << 30) | ((k as u64) << 21) | ((l as u64) << 12);
                    page_table_1[l].set_addr(PhysAddr::new(addr), Flags::PRESENT | Flags::WRITABLE);
                }

                page_table_2[k].set_addr(page_table_1_frame.start_address(), Flags::PRESENT | Flags::WRITABLE);
            }

            page_table_3[j].set_addr(page_table_2_frame.start_address(), Flags::PRESENT | Flags::WRITABLE);
        }

        level_4_table[i].set_addr(page_table_3_frame.start_address(), Flags::PRESENT | Flags::WRITABLE);
    }

    // Set the new CR3
    unsafe { Cr3::write(level_4_frame, Cr3Flags::empty()) };

    // Reinitialize the mapper
    let mapper = unsafe { petroleum::page_table::init(VirtAddr::new(0)) };
    *MAPPER.get().unwrap().lock() = mapper;
}

// Allocate heap from memory map (find virtual address from physical)
pub fn allocate_heap_from_map(phys_start: PhysAddr, _size: usize) -> VirtAddr {
    let memory_map = *MEMORY_MAP.get().unwrap();
    for desc in memory_map {
        if desc.physical_start == phys_start.as_u64() {
            return VirtAddr::new(desc.virtual_start);
        }
    }
    // Fallback to identity mapping
    VirtAddr::new(phys_start.as_u64())
}
