use core::{alloc::{GlobalAlloc, Layout}, ptr::NonNull};
use x86_64::{PhysAddr, VirtAddr};

pub const HEAP_SIZE: usize = 100 * 1024; // 100 KiB

#[repr(C)]
struct ListNode {
    size: usize,
    next: Option<&'static mut ListNode>,
}

pub struct Heap {
    bottom: usize,
    size: usize,
    used: usize,
    next: usize,
}

fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

impl Heap {
    pub const fn empty() -> Self {
        Heap {
            bottom: 0,
            size: 0,
            used: 0,
            next: 0,
        }
    }

    pub unsafe fn init(&mut self, heap_start: *mut u8, heap_size: usize) {
        self.bottom = heap_start as usize;
        self.size = heap_size;
        self.used = 0;
        self.next = self.bottom;
    }

    fn alloc(&mut self, layout: Layout) -> *mut u8 {
        let alloc_start = align_up(self.next, layout.align());
        self.next = alloc_start + layout.size();
        if self.next > self.bottom + self.size {
            core::ptr::null_mut()
        } else {
            self.used += layout.size();
            alloc_start as *mut u8
        }
    }

    fn dealloc(&mut self, ptr: *mut u8, layout: Layout) {
        // Simple bump allocator - only dealloc the last allocation
        let ptr_usize = ptr as usize;
        let alloc_end = ptr_usize + layout.size();
        if alloc_end == self.next {
            self.next = ptr_usize;
            self.used -= layout.size();
        }
        // Otherwise, leak the memory (acceptable for early boot)
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

pub fn init(heap_start: VirtAddr, heap_size: usize) {
    unsafe {
        let heap_start = heap_start.as_mut_ptr::<u8>();
        ALLOCATOR.lock().init(heap_start, heap_size);
    }
}

// Allocate heap from memory map (simplified: assume identity mapping for now)
pub fn allocate_heap_from_map(phys_start: PhysAddr, _size: usize) -> VirtAddr {
    // Simplified: assume identity mapping (implement proper page table later)
    VirtAddr::new(phys_start.as_u64())
}
