//! Convenience Functions for Memory Management

use super::*;

fn with_manager<T>(f: impl FnOnce(&mut UnifiedMemoryManager) -> SystemResult<T>) -> SystemResult<T> {
    MEMORY_MANAGER.lock().as_mut().ok_or(SystemError::InternalError).and_then(f)
}

/// Allocate kernel memory pages
pub fn allocate_kernel_pages(count: usize) -> SystemResult<usize> {
    with_manager(|m| m.allocate_pages(count))
}

/// Free kernel memory pages
pub fn free_kernel_pages(address: usize, count: usize) -> SystemResult<()> {
    with_manager(|m| m.free_pages(address, count))
}

/// Map memory for device I/O
pub fn map_mmio(physical_addr: usize, size: usize) -> SystemResult<usize> {
    with_manager(|manager| {
        let frame_count = (size + 4095) / 4096;
        let virtual_addr = manager.allocate_pages(frame_count)?;
        for i in 0..frame_count {
            manager.map_address(virtual_addr + i * 4096, physical_addr + i * 4096, 1)?;
        }
        Ok(virtual_addr)
    })
}

/// Unmap memory-mapped I/O
pub fn unmap_mmio(virtual_addr: usize, size: usize) -> SystemResult<()> {
    with_manager(|manager| {
        let frame_count = (size + 4095) / 4096;
        manager.unmap_address(virtual_addr, frame_count)
    })
}
