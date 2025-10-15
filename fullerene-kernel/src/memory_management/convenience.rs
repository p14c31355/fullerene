//! Convenience Functions for Memory Management
//!
//! This module provides high-level convenience functions for common memory management operations.

use super::*;

/// Convenience functions for memory management
pub mod convenience {
    use super::*;

    /// Allocate kernel memory pages
    pub fn allocate_kernel_pages(count: usize) -> SystemResult<usize> {
        if let Some(manager) = MEMORY_MANAGER.lock().as_mut() {
            manager.allocate_pages(count)
        } else {
            Err(SystemError::InternalError)
        }
    }

    /// Free kernel memory pages
    pub fn free_kernel_pages(address: usize, count: usize) -> SystemResult<()> {
        if let Some(manager) = MEMORY_MANAGER.lock().as_mut() {
            manager.free_pages(address, count)
        } else {
            Err(SystemError::InternalError)
        }
    }

    /// Map memory for device I/O
    pub fn map_mmio(physical_addr: usize, size: usize) -> SystemResult<usize> {
        if let Some(manager) = MEMORY_MANAGER.lock().as_mut() {
            let frame_count = (size + 4095) / 4096;
            let virtual_addr = manager.allocate_pages(frame_count)?;

            for i in 0..frame_count {
                let phys_addr = physical_addr + (i * 4096);
                let virt_addr = virtual_addr + (i * 4096);

                manager.map_address(virt_addr, phys_addr, 1)?;
            }

            Ok(virtual_addr)
        } else {
            Err(SystemError::InternalError)
        }
    }

    /// Unmap memory-mapped I/O
    pub fn unmap_mmio(virtual_addr: usize, size: usize) -> SystemResult<()> {
        if let Some(manager) = MEMORY_MANAGER.lock().as_mut() {
            let frame_count = (size + 4095) / 4096;
            manager.unmap_address(virtual_addr, frame_count)
        } else {
            Err(SystemError::InternalError)
        }
    }
}
