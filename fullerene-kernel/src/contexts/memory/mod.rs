//! MemoryContext — page table, physical/virtual allocators, heap, DMA.

use crate::memory_management::UnifiedMemoryManager;
use petroleum::initializer::FrameAllocator;
use petroleum::page_table::BootInfoFrameAllocator;

pub struct MemoryContext {
    pub manager: Option<UnifiedMemoryManager>,
    pub frame_allocator: Option<BootInfoFrameAllocator>,
    pub initialized: bool,
}

impl MemoryContext {
    pub const fn new() -> Self {
        Self {
            manager: None,
            frame_allocator: None,
            initialized: false,
        }
    }
    pub fn is_ready(&self) -> bool {
        self.manager.is_some() && self.initialized
    }
    pub fn allocate_frame(&mut self) -> Result<usize, &'static str> {
        self.mgr()?.allocate_frame().map_err(|_| "FrameAllocFailed")
    }
    pub fn free_frame(&mut self, phys: usize) -> Result<(), &'static str> {
        self.mgr()?.free_frame(phys).map_err(|_| "FreeFailed")
    }
    pub fn allocate_contiguous(&mut self, n: usize) -> Result<usize, &'static str> {
        self.mgr()?
            .allocate_contiguous_frames(n)
            .map_err(|_| "ContiguousAllocFailed")
    }
    pub fn map_page(
        &mut self,
        v: usize,
        p: usize,
        f: x86_64::structures::paging::PageTableFlags,
    ) -> Result<(), &'static str> {
        self.mgr()?.safe_map_page(v, p, f).map_err(|_| "MapFailed")
    }
    pub fn map_mmio(&mut self, phys: usize, virt: usize, size: usize) -> Result<(), &'static str> {
        self.mgr()?
            .map_mmio_region(phys, virt, size)
            .map_err(|_| "MMIOMapFailed")
    }
    pub unsafe fn extend_heap(&self, additional: usize) -> Result<(), ()> {
        unsafe { crate::heap::extend_kernel_heap(additional) }
    }
    pub fn heap_stats(&self) -> petroleum::HeapStats {
        petroleum::heap_stats()
    }
    fn mgr(&mut self) -> Result<&mut UnifiedMemoryManager, &'static str> {
        self.manager.as_mut().ok_or("Not init")
    }
}

crate::define_context!(MemoryContext, memory, MEM_CTX);
