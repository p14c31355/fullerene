//! MemoryContext — page table, physical/virtual allocators, heap, DMA.
//!
//! Aggregates:
//! - `PhysicalMemoryContext`  — frame allocation, MMIO mapping
//! - `VirtualMemoryContext`   — page tables, address-space management
//! - `DmaContext`             — contiguous DMA buffers
//! - `HeapContext`            — kernel heap tracking

pub mod physical;
pub mod virtual_mem;
pub mod dma;
pub mod heap;

use spin::Mutex;

use crate::memory_management::UnifiedMemoryManager;
use petroleum::initializer::FrameAllocator;
use petroleum::page_table::BootInfoFrameAllocator;

pub use physical::PhysicalMemoryContext;
pub use virtual_mem::VirtualMemoryContext;
pub use dma::DmaContext;
pub use heap::HeapContext;

/// MemoryContext — aggregate of all memory subsystems.
///
/// Replaces the former thin `MemoryContext` that only held
/// `manager`, `frame_allocator`, and `initialized`.
pub struct MemoryContext {
    pub physical: PhysicalMemoryContext,
    pub virtual_memory: VirtualMemoryContext,
    pub dma: DmaContext,
    pub heap: HeapContext,

    // ── retained for backward compat during migration ─────────
    pub manager: Option<UnifiedMemoryManager>,
    pub frame_allocator: Option<BootInfoFrameAllocator>,
    pub initialized: bool,
}

impl MemoryContext {
    pub const fn new() -> Self {
        Self {
            physical: PhysicalMemoryContext::new(),
            virtual_memory: VirtualMemoryContext::new(),
            dma: DmaContext::new(),
            heap: HeapContext::new(),
            manager: None,
            frame_allocator: None,
            initialized: false,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.manager.is_some() && self.initialized
    }

    // ── delegation (for backward compat during migration) ─────
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

// ── Global singleton ──────────────────────────────────────────
static MEM_CTX: Mutex<Option<MemoryContext>> = Mutex::new(None);

pub fn init_memory() {
    *MEM_CTX.lock() = Some(MemoryContext::new());
}

pub fn get_memory() -> &'static Mutex<Option<MemoryContext>> {
    &MEM_CTX
}

pub fn with_memory_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut MemoryContext) -> R,
{
    MEM_CTX.lock().as_mut().map(f)
}

pub fn with_memory<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&MemoryContext) -> R,
{
    MEM_CTX.lock().as_ref().map(f)
}