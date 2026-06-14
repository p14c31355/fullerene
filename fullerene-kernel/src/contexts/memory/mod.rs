//! MemoryContext — page table, physical/virtual allocators, heap, DMA.
//!
//! Now wraps `VirtualMemoryContext` for centralised mapping tracking
//! while keeping backward-compatible `UnifiedMemoryManager` access.

use crate::memory_management::UnifiedMemoryManager;
use petroleum::initializer::FrameAllocator;
use petroleum::page_table::virtual_memory::VirtualMemoryContext;
use petroleum::page_table::{BitmapFrameAllocator, BootInfoFrameAllocator};

pub struct MemoryContext {
    pub manager: Option<UnifiedMemoryManager>,
    pub frame_allocator: Option<BootInfoFrameAllocator>,
    /// Centralised virtual-memory state (owned).
    pub vm: Option<VirtualMemoryContext>,
    pub initialized: bool,
}

impl MemoryContext {
    pub const fn new() -> Self {
        Self {
            manager: None,
            frame_allocator: None,
            vm: None,
            initialized: false,
        }
    }
    pub fn is_ready(&self) -> bool {
        self.manager.is_some() && self.initialized
    }

    /// Initialise the virtual-memory context from boot-phase state.
    ///
    /// The `BitmapFrameAllocator` is **moved** in; the caller must not
    /// use it afterwards.
    pub fn init_vm(
        &mut self,
        pml4_phys: u64,
        physical_offset: u64,
        allocator: BitmapFrameAllocator,
    ) {
        self.vm = Some(VirtualMemoryContext::new(pml4_phys, physical_offset, allocator));
    }

    // ── High-level VM operations (preferred) ─────────────────

    pub fn map_framebuffer_vm(&mut self, phys: u64, size: u64) -> Result<u64, &'static str> {
        self.vm_mut()?.map_framebuffer(phys, size, None)
    }

    pub fn map_heap_vm(&mut self, phys: u64, size: u64) -> Result<u64, &'static str> {
        self.vm_mut()?.map_heap(phys, size)
    }

    pub fn direct_map_physical_vm(
        &mut self,
        map: &[petroleum::page_table::memory_map::MemoryMapDescriptor],
    ) -> Result<(), &'static str> {
        self.vm_mut()?.direct_map_physical(map)
    }

    pub fn clone_for_process_vm(&mut self) -> Result<VirtualMemoryContext, &'static str> {
        self.vm_mut()?.clone_for_process()
    }

    pub fn framebuffer_virt(&self) -> Option<u64> {
        self.vm()?.framebuffer_mapping().map(|m| m.virt_start)
    }

    pub fn physical_offset(&self) -> Option<u64> {
        Some(self.vm()?.physical_offset)
    }

    fn vm(&self) -> Option<&VirtualMemoryContext> {
        self.vm.as_ref()
    }

    fn vm_mut(&mut self) -> Result<&mut VirtualMemoryContext, &'static str> {
        self.vm.as_mut().ok_or("VM not initialised")
    }

    // ── Legacy operations (backward-compatible) ──────────────

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