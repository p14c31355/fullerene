//! Dynamic memory allocator implementation
//!
//! This module provides the heap allocator for the kernel.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;
use spin::Mutex;
use x86_64::{PhysAddr, VirtAddr};

/// Linked list node for free memory blocks
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

/// Heap allocator structure
pub struct Heap {
    head: *mut ListNode,
}

// SAFETY: This is a single-threaded kernel allocator
unsafe impl Send for Heap {}

impl Heap {
    /// Create empty heap
    pub const fn empty() -> Self {
        Heap {
            head: ptr::null_mut(),
        }
    }

    /// Initialize heap with memory region
    pub unsafe fn init(&mut self, heap_start: *mut u8, heap_size: usize) {
        let node = heap_start as *mut ListNode;
        unsafe { *node = ListNode::new(heap_size); }
        self.head = node;
    }

    /// Allocate memory
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
                    // Split block if needed
                    let remaining = node.end_addr() - alloc_end;
                    if remaining > core::mem::size_of::<ListNode>() {
                        let new_node = alloc_end as *mut ListNode;
                        unsafe { *new_node = ListNode::new(remaining); }
                        unsafe { (*new_node).next = node.next; }
                        node.next = new_node;
                    }
                    node.size = padding;
                    if node.size == 0 {
                        *current = node.next;
                    }
                    return alloc_start as *mut u8;
                }
                current = &mut node.next;
            }
        }
        ptr::null_mut()
    }

    /// Deallocate memory
    fn dealloc(&mut self, ptr: *mut u8, layout: Layout) {
        let size = layout.size();

        unsafe {
            let new_node = ptr as *mut ListNode;
            *new_node = ListNode::new(size);
            self.insert_sorted(new_node);
            self.coalesce();
        }
    }

    /// Insert node in sorted order
    unsafe fn insert_sorted(&mut self, new_node: *mut ListNode) {
        if self.head.is_null() || unsafe { (*new_node).start_addr() < (*self.head).start_addr() } {
            unsafe { (*new_node).next = self.head; }
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

    /// Coalesce adjacent free blocks
    unsafe fn coalesce(&mut self) {
        if self.head.is_null() {
            return;
        }

        let mut current = self.head;
        while !unsafe { (*current).next.is_null() } {
            unsafe {
                let next = (*current).next;
                if (*current).end_addr() == (*next).start_addr() {
                    (*current).size += (*next).size;
                    (*current).next = (*next).next;
                } else {
                    current = next;
                }
            }
        }
    }
}

/// Helper function to align addresses
fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

/// Locked heap wrapper for thread safety
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

// SAFETY: Locked<Heap> is safe to use as GlobalAlloc
unsafe impl GlobalAlloc for Locked<Heap> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.inner.lock().alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.inner.lock().dealloc(ptr, layout)
    }
}

/// Heap size constant
pub const HEAP_SIZE: usize = 100 * 1024; // 100 KiB

/// Global heap allocator instance
#[global_allocator]
pub static ALLOCATOR: Locked<Heap> = Locked::new(Heap::empty());
