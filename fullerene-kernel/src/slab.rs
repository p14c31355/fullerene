//! Slab allocator — fixed-size object pool for the kernel.
//!
//! Provides O(1) allocation/deallocation of fixed-size objects from
//! pre-allocated pages.  Useful for small, frequently-allocated kernel
//! structures (process descriptors, file handles, socket buffers).
//!
//! # Design
//!
//! - Each slab caches objects of a single size (power of two, 16…4096).
//! - A slab page is a 4 KiB frame divided into equal‑sized slots.
//! - Free slots form a singly‑linked list (LIFO stack) within the page itself.
//! - When a slab is exhausted a new page is allocated from the frame allocator.
//! - When a page becomes entirely free it is returned to the frame allocator.

use alloc::vec::Vec;
use spin::Mutex;
use x86_64::structures::paging::PageTableFlags;
use petroleum::initializer::FrameAllocator;

use crate::memory_management;

/// Supported object sizes (powers of two from 16 to 4096).
const NUM_CACHES: usize = 9;
const CACHE_SIZES: [usize; NUM_CACHES] = [16, 32, 64, 128, 256, 512, 1024, 2048, 4096];

/// A single slab page (4 KiB frame).
struct SlabPage {
    /// Physical address of the page frame.
    phys: usize,
    /// Virtual address of the mapped page.
    virt: usize,
    /// Object size for this slab.
    obj_size: usize,
    /// Head of the free‑list (offset within the page, or `core::usize::MAX` if none).
    free_head: usize,
    /// Number of allocated objects in this page.
    allocated: usize,
    /// Maximum number of objects per page.
    capacity: usize,
}

impl SlabPage {
    /// Create a new slab page for objects of `obj_size`.
    fn new(obj_size: usize) -> Option<Self> {
        let phys = {
            let mut mgr = memory_management::get_memory_manager().lock();
            mgr.as_mut()?.allocate_frame().ok()?
        };
        let virt = petroleum::common::memory::physical_to_virtual(phys);
        {
            let mut mgr = memory_management::get_memory_manager().lock();
            let m = mgr.as_mut()?;
            m.safe_map_page(virt, phys,
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE).ok()?;
        }

        let capacity = 4096 / obj_size;
        let mut this = Self {
            phys,
            virt,
            obj_size,
            free_head: usize::MAX,
            allocated: 0,
            capacity,
        };

        // Build the free list: each slot stores the offset of the next free slot.
        for i in 0..capacity {
            let offset = i * obj_size;
            unsafe {
                let ptr = (virt + offset) as *mut usize;
                ptr.write(if i + 1 < capacity { (i + 1) * obj_size } else { usize::MAX });
            }
        }
        this.free_head = 0;

        Some(this)
    }

    /// Allocate one object from this page. Returns a virtual address.
    fn alloc(&mut self) -> Option<usize> {
        if self.free_head == usize::MAX {
            return None;
        }
        let offset = self.free_head;
        self.free_head = unsafe { *((self.virt + offset) as *const usize) };
        self.allocated += 1;
        Some(self.virt + offset)
    }

    /// Free one object.
    fn free(&mut self, addr: usize) {
        let offset = addr - self.virt;
        debug_assert!(offset < 4096);
        debug_assert!(self.allocated > 0);
        unsafe {
            let slot = (self.virt + offset) as *mut usize;
            slot.write(self.free_head);
        }
        self.free_head = offset;
        self.allocated -= 1;
    }

    fn is_empty(&self) -> bool {
        self.allocated == 0
    }
}

impl Drop for SlabPage {
    fn drop(&mut self) {
        if let Some(mgr) = memory_management::get_memory_manager().lock().as_mut() {
            let _ = mgr.safe_unmap_page(self.virt);
            let _ = mgr.free_frame(self.phys);
        }
    }
}

/// A cache of fixed-size objects backed by one or more slab pages.
struct SlabCache {
    obj_size: usize,
    pages: Vec<SlabPage>,
    partial_hint: usize,
}

impl SlabCache {
    const fn new(obj_size: usize) -> Self {
        Self { obj_size, pages: Vec::new(), partial_hint: 0 }
    }

    fn alloc(&mut self) -> Option<usize> {
        let start = self.partial_hint.min(self.pages.len());
        for i in start..self.pages.len() {
            if self.pages[i].free_head != usize::MAX {
                self.partial_hint = i;
                return self.pages[i].alloc();
            }
        }
        for i in 0..start {
            if self.pages[i].free_head != usize::MAX {
                self.partial_hint = i;
                return self.pages[i].alloc();
            }
        }
        let mut page = SlabPage::new(self.obj_size)?;
        let addr = page.alloc();
        self.pages.push(page);
        self.partial_hint = self.pages.len() - 1;
        addr
    }

    fn free(&mut self, addr: usize) {
        let page_start = addr & !4095;
        for (i, page) in self.pages.iter_mut().enumerate() {
            if page.virt == page_start {
                page.free(addr);
                self.partial_hint = i;
                return;
            }
        }
    }

    fn reap(&mut self) {
        self.pages.retain(|page| !page.is_empty());
        self.partial_hint = 0;
    }
}

// ── Global slab state ─────────────────────────────────────────────

/// Global slab caches, initialized lazily.
static SLABS: Mutex<Option<[SlabCache; NUM_CACHES]>> = Mutex::new(None);

fn get_slabs() -> &'static mut [SlabCache; NUM_CACHES] {
    let mut guard = SLABS.lock();
    if guard.is_none() {
        *guard = Some([
            SlabCache::new(16),
            SlabCache::new(32),
            SlabCache::new(64),
            SlabCache::new(128),
            SlabCache::new(256),
            SlabCache::new(512),
            SlabCache::new(1024),
            SlabCache::new(2048),
            SlabCache::new(4096),
        ]);
    }
    // SAFETY: we hold the lock and just ensured Some.
    unsafe {
        let ptr = guard.as_mut().unwrap_unchecked() as *mut [SlabCache; NUM_CACHES];
        &mut *ptr
    }
}

fn size_to_class(size: usize) -> usize {
    for (i, &s) in CACHE_SIZES.iter().enumerate() {
        if size <= s { return i; }
    }
    NUM_CACHES - 1
}

// ── Public API ────────────────────────────────────────────────────

pub fn alloc(size: usize) -> Option<usize> {
    let idx = size_to_class(size);
    get_slabs()[idx].alloc()
}

pub unsafe fn free(addr: usize, size: usize) {
    let idx = size_to_class(size);
    get_slabs()[idx].free(addr);
}

pub fn reap() {
    for cache in get_slabs().iter_mut() {
        cache.reap();
    }
}