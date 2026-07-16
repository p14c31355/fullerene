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
        self.vm = Some(VirtualMemoryContext::new(
            pml4_phys,
            physical_offset,
            allocator,
        ));
    }

    // ── High-level VM operations (preferred) ─────────────────

    pub fn map_framebuffer_vm(
        &mut self,
        phys: u64,
        size: u64,
    ) -> Result<u64, petroleum::MemoryError> {
        self.vm_mut()?.map_framebuffer(phys, size, None)
    }

    pub fn map_heap_vm(&mut self, phys: u64, size: u64) -> Result<u64, petroleum::MemoryError> {
        self.vm_mut()?.map_heap(phys, size)
    }

    pub fn direct_map_physical_vm(
        &mut self,
        map: &[petroleum::page_table::memory_map::MemoryMapDescriptor],
    ) -> Result<(), petroleum::MemoryError> {
        self.vm_mut()?.direct_map_physical(map)
    }

    pub fn clone_for_process_vm(&mut self) -> Result<VirtualMemoryContext, petroleum::MemoryError> {
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

    fn vm_mut(&mut self) -> Result<&mut VirtualMemoryContext, petroleum::MemoryError> {
        self.vm
            .as_mut()
            .ok_or(petroleum::MemoryError::NotInitialized)
    }

    // ── Legacy operations (backward-compatible) ──────────────

    pub fn allocate_frame(&mut self) -> Result<usize, petroleum::MemoryError> {
        self.mgr()?
            .allocate_frame()
            .map_err(|_| petroleum::MemoryError::FrameAllocationFailed)
    }
    pub fn free_frame(&mut self, phys: usize) -> Result<(), petroleum::MemoryError> {
        self.mgr()?
            .free_frame(phys)
            .map_err(|_| petroleum::MemoryError::UnmappingFailed)
    }
    pub fn allocate_contiguous(&mut self, n: usize) -> Result<usize, petroleum::MemoryError> {
        self.mgr()?
            .allocate_contiguous_frames(n)
            .map_err(|_| petroleum::MemoryError::FrameAllocationFailed)
    }
    pub fn map_page(
        &mut self,
        v: usize,
        p: usize,
        f: x86_64::structures::paging::PageTableFlags,
    ) -> Result<(), petroleum::MemoryError> {
        self.mgr()?
            .safe_map_page(v, p, f)
            .map_err(|_| petroleum::MemoryError::MappingFailed)
    }
    pub fn map_mmio(
        &mut self,
        phys: usize,
        virt: usize,
        size: usize,
    ) -> Result<(), petroleum::MemoryError> {
        self.mgr()?
            .map_mmio_region(phys, virt, size)
            .map_err(|_| petroleum::MemoryError::MappingFailed)
    }
    pub unsafe fn extend_heap(&self, additional: usize) -> Result<(), ()> {
        unsafe { crate::heap::extend_kernel_heap(additional) }
    }
    pub fn heap_stats(&self) -> petroleum::HeapStats {
        petroleum::heap_stats()
    }
    fn mgr(&mut self) -> Result<&mut UnifiedMemoryManager, petroleum::MemoryError> {
        self.manager
            .as_mut()
            .ok_or(petroleum::MemoryError::NotInitialized)
    }
}

crate::define_context!(MemoryContext, memory, MEM_CTX);
