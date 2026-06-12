//! MemoryContext — Unified memory management context.
//!
//! Consolidates:
//! - Page table (level-4, mappings)
//! - Physical frame allocator (bitmap)
//! - Virtual address allocator
//! - Heap state
//!
//! Previously these were scattered across:
//! - `crate::memory_management::MEMORY_MANAGER`
//! - `crate::heap::FRAME_ALLOCATOR`
//! - `petroleum::page_table::ALLOCATOR`
//! - `petroleum::page_table::constants::get_frame_allocator_mut()`
//!
//! # Design
//!
//! ```rust,ignore
//! memory.allocate_frame()?;
//! memory.map_page(virt, phys, flags);
//! memory.extend_heap(additional);
//! ```
//!
//! This eliminates the tight coupling where `map_framebuffer()`
//! had to know about `alloc_page()` internals.

use crate::memory_management::UnifiedMemoryManager;
use petroleum::initializer::FrameAllocator;
use petroleum::page_table::BootInfoFrameAllocator;
use spin::Mutex;

/// Memory management context.
///
/// Owns the global memory manager and frame allocator so callers
/// don't need to touch global statics directly.
pub struct MemoryContext {
    /// The unified memory manager (page tables, mappings, allocator).
    pub manager: Option<UnifiedMemoryManager>,

    /// Boot-info frame allocator (used during early init).
    pub frame_allocator: Option<BootInfoFrameAllocator>,

    /// Whether the memory subsystem has been fully initialised.
    pub initialized: bool,
}

impl MemoryContext {
    /// Create an empty memory context.
    pub const fn new() -> Self {
        Self {
            manager: None,
            frame_allocator: None,
            initialized: false,
        }
    }

    /// Returns `true` when the memory manager is set and ready.
    pub fn is_ready(&self) -> bool {
        self.manager.is_some() && self.initialized
    }

    /// Allocate a physical frame (4 KiB).
    pub fn allocate_frame(&mut self) -> Result<usize, &'static str> {
        let mgr = self.manager.as_mut().ok_or("MemoryManager not initialized")?;
        mgr.allocate_frame().map_err(|_| "FrameAllocationFailed")
    }

    /// Free a physical frame.
    pub fn free_frame(&mut self, phys: usize) -> Result<(), &'static str> {
        let mgr = self.manager.as_mut().ok_or("MemoryManager not initialized")?;
        mgr.free_frame(phys).map_err(|_| "FreeFailed")
    }

    /// Allocate contiguous physical frames.
    pub fn allocate_contiguous_frames(&mut self, count: usize) -> Result<usize, &'static str> {
        let mgr = self.manager.as_mut().ok_or("MemoryManager not initialized")?;
        mgr.allocate_contiguous_frames(count)
            .map_err(|_| "ContiguousAllocFailed")
    }

    /// Map a physical page to a virtual address.
    pub fn map_page(
        &mut self,
        virt: usize,
        phys: usize,
        flags: x86_64::structures::paging::PageTableFlags,
    ) -> Result<(), &'static str> {
        let mgr = self.manager.as_mut().ok_or("MemoryManager not initialized")?;
        mgr.safe_map_page(virt, phys, flags)
            .map_err(|_| "MapFailed")
    }

    /// Map an MMIO region.
    pub fn map_mmio_region(
        &mut self,
        phys: usize,
        virt: usize,
        size: usize,
    ) -> Result<(), &'static str> {
        let mgr = self.manager.as_mut().ok_or("MemoryManager not initialized")?;
        mgr.map_mmio_region(phys, virt, size)
            .map_err(|_| "MMIOMapFailed")
    }

    /// Create a new process page table (returns PML4 physical address).
    pub fn create_process_page_table(&mut self) -> Result<usize, &'static str> {
        let mgr = self.manager.as_mut().ok_or("MemoryManager not initialized")?;
        let pml4_frame = mgr
            .allocate_frame()
            .map_err(|_| "FrameAllocationFailed")?;

        let pml4_virt = petroleum::common::memory::physical_to_virtual(pml4_frame);
        unsafe {
            let table_ptr = pml4_virt as *mut u64;
            core::slice::from_raw_parts_mut(table_ptr, 512).fill(0);
        }

        // Copy kernel mappings (entries 256..512)
        let current_cr3 = x86_64::registers::control::Cr3::read();
        let kernel_table_phys = current_cr3.0.start_address().as_u64() as usize;
        let kernel_table_virt = petroleum::common::memory::physical_to_virtual(kernel_table_phys);
        unsafe {
            let src = (kernel_table_virt + 256 * 8) as *const u64;
            let dst = (pml4_virt + 256 * 8) as *mut u64;
            core::ptr::copy_nonoverlapping(src, dst, 256);
        }
        Ok(pml4_frame)
    }

    /// Extend the kernel heap by `additional` bytes.
    ///
    /// # Safety
    ///
    /// Must only be called after the allocator is initialized.
    pub unsafe fn extend_heap(&self, additional: usize) -> Result<(), ()> {
        unsafe { crate::heap::extend_kernel_heap(additional) }
    }

    /// Get current heap statistics (used, total, free).
    pub fn heap_stats(&self) -> petroleum::HeapStats {
        petroleum::heap_stats()
    }
}

/// Global memory context.
static MEMORY_CONTEXT: Mutex<Option<MemoryContext>> = Mutex::new(None);

/// Initialise the global memory context.
pub fn init_memory_context() {
    *MEMORY_CONTEXT.lock() = Some(MemoryContext::new());
}

/// Mark the memory context as initialised.
pub fn set_memory_ready(manager: UnifiedMemoryManager) {
    let mut guard = MEMORY_CONTEXT.lock();
    if let Some(ctx) = guard.as_mut() {
        ctx.manager = Some(manager);
        ctx.initialized = true;
    }
}

/// Get a reference to the global memory context.
pub fn get_memory() -> &'static Mutex<Option<MemoryContext>> {
    &MEMORY_CONTEXT
}

/// Convenience: execute a closure with a mutable reference.
pub fn with_memory_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut MemoryContext) -> R,
{
    MEMORY_CONTEXT.lock().as_mut().map(f)
}

/// Convenience: execute a closure with a shared reference.
pub fn with_memory<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&MemoryContext) -> R,
{
    MEMORY_CONTEXT.lock().as_ref().map(f)
}