//! ANE memory model: flat translation (the offset stays 0 on the phone).
//!
//! The ANE kernel runs from the loader's flat mappings, so virtual and
//! physical addresses coincide; this file carries the small translation core
//! that the shared code (e.g. `PageBuf`) relies on.

use core::sync::atomic::{AtomicUsize, Ordering};

/// Heap start address, set once during bring-up.
pub static HEAP_START: AtomicUsize = AtomicUsize::new(0);

/// Heap end address (start + size).
pub static HEAP_END: AtomicUsize = AtomicUsize::new(0);

/// Virtual-to-physical offset; flat on the ANE, so this stays 0.
pub static PHYSICAL_MEMORY_OFFSET: AtomicUsize = AtomicUsize::new(0);

/// Set heap range for allocator-related page fault detection.
pub fn set_heap_range(start: usize, size: usize) {
    HEAP_START.store(start, Ordering::SeqCst);
    HEAP_END.store(start.saturating_add(size), Ordering::SeqCst);
}

/// Get the current heap range (start, end).
pub fn get_heap_range() -> (usize, usize) {
    (HEAP_START.load(Ordering::SeqCst), HEAP_END.load(Ordering::SeqCst))
}

/// Set the physical memory offset for virtual-to-physical translation.
pub fn set_physical_memory_offset(offset: usize) {
    PHYSICAL_MEMORY_OFFSET.store(offset, Ordering::Relaxed);
}

/// Get the physical memory offset for virtual-to-physical translation.
pub fn get_physical_memory_offset() -> usize {
    PHYSICAL_MEMORY_OFFSET.load(Ordering::Relaxed)
}

/// Convert a virtual address to a physical address using the offset.
pub fn virtual_to_physical(virtual_addr: usize) -> usize {
    virtual_addr - get_physical_memory_offset()
}

/// Convert a physical address to a virtual address using the offset.
pub fn physical_to_virtual(physical_addr: usize) -> usize {
    physical_addr + get_physical_memory_offset()
}

/// Safely create a slice from a physical address and length.
///
/// # Safety
///
/// The caller must ensure the physical memory range is mapped and accessible.
pub unsafe fn phys_to_slice(phys_addr: usize, len: usize) -> &'static [u8] {
    unsafe {
        let virt_addr = physical_to_virtual(phys_addr);
        core::slice::from_raw_parts(virt_addr as *const u8, len)
    }
}

/// Safely create a mutable slice from a physical address and length.
///
/// # Safety
///
/// The caller must ensure the physical memory range is mapped and accessible.
pub unsafe fn phys_to_slice_mut(phys_addr: usize, len: usize) -> &'static mut [u8] {
    unsafe {
        let virt_addr = physical_to_virtual(phys_addr);
        core::slice::from_raw_parts_mut(virt_addr as *mut u8, len)
    }
}
