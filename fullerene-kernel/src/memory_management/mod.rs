//! Unified Memory Management Implementation
//!
//! This module provides a comprehensive memory management system that implements
//! the MemoryManager, ProcessMemoryManager, PageTableHelper, and FrameAllocator traits.

use spin::Mutex;

use petroleum::common::logging::{SystemError, SystemResult};
use petroleum::initializer::{FrameAllocator, Initializable, MemoryManager};
use petroleum::mem_debug;
use x86_64::structures::paging::{
    FrameAllocator as X86FrameAllocator, PageTableFlags as PageFlags,
};

use petroleum::page_table::process::ProcessPageTable;
pub mod convenience;
pub mod kernel_space;
pub mod manager;
pub mod process_memory;

pub use manager::UnifiedMemoryManager;
pub use petroleum::page_table::*;
pub use process_memory::*;

// Memory management error types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocError {
    OutOfMemory,
    MappingFailed,
}

petroleum::error_chain!(AllocError, petroleum::common::logging::SystemError,
    AllocError::OutOfMemory => petroleum::common::logging::SystemError::MemOutOfMemory,
    AllocError::MappingFailed => petroleum::common::logging::SystemError::MappingFailed,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreeError {
    UnmappingFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    MappingFailed,
    UnmappingFailed,
    FrameAllocationFailed,
}

petroleum::error_chain!(MapError, petroleum::common::logging::SystemError,
    MapError::MappingFailed => petroleum::common::logging::SystemError::MappingFailed,
    MapError::UnmappingFailed => petroleum::common::logging::SystemError::UnmappingFailed,
    MapError::FrameAllocationFailed => petroleum::common::logging::SystemError::FrameAllocationFailed,
);

petroleum::error_chain!(FreeError, petroleum::common::logging::SystemError,
    FreeError::UnmappingFailed => petroleum::common::logging::SystemError::UnmappingFailed,
);

// Global memory manager instance
static MEMORY_MANAGER: Mutex<Option<UnifiedMemoryManager>> = Mutex::new(None);

/// Switch to a specific page table
pub fn switch_to_page_table(page_table: &ProcessPageTable) -> SystemResult<()> {
    let pml4_frame = page_table.pml4_frame().ok_or(SystemError::InternalError)?;
    petroleum::safe_cr3_write!(pml4_frame);
    Ok(())
}

/// Create a new process page table
pub fn create_process_page_table() -> SystemResult<ProcessPageTable> {
    mem_debug!("Mem: create_process_page_table start\n");

    // Allocate a new PML4 frame for the process page table
    let pml4_phys = {
        let mut manager_guard = get_memory_manager().lock();
        let manager = manager_guard.as_mut().ok_or(SystemError::InternalError)?;
        manager
            .allocate_frame()
            .map_err(|_| SystemError::FrameAllocationFailed)?
    };

    let pml4_frame = x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(
        x86_64::PhysAddr::new(pml4_phys as u64),
    );

    // Zero the allocated page table frame using Direct Mapping
    let pml4_virt = petroleum::common::memory::physical_to_virtual(pml4_phys);

    unsafe {
        let table_ptr = pml4_virt as *mut u64;
        core::slice::from_raw_parts_mut(table_ptr, 512).fill(0);
    }

    // Copy kernel mappings to the new page table (PML4[256..512])
    let current_cr3 = x86_64::registers::control::Cr3::read();
    let kernel_table_phys = current_cr3.0.start_address().as_u64() as usize;
    let kernel_table_virt = petroleum::common::memory::physical_to_virtual(kernel_table_phys);

    unsafe {
        let kernel_entries_src = (kernel_table_virt + 256 * 8) as *const u64;
        let new_entries_dst = (pml4_virt + 256 * 8) as *mut u64;
        core::ptr::copy_nonoverlapping(kernel_entries_src, new_entries_dst, 256);
    }

    // Initialize the new page table manager with the allocated frame
    let mut page_table_manager = ProcessPageTable::new_with_frame(pml4_frame);
    Initializable::init(&mut page_table_manager)?;

    mem_debug!("Mem: create_process_page_table done\n");
    Ok(page_table_manager)
}

/// Deallocate a process page table and free its frames
pub fn deallocate_process_page_table(pml4_frame: x86_64::structures::paging::PhysFrame) {
    if let Some(manager) = MEMORY_MANAGER.lock().as_mut() {
        let frame_addr = pml4_frame.start_address().as_u64() as usize;
        let _ = manager.free_frame(frame_addr);
        mem_debug!("Mem: Deallocated process page table\n");
    }
}

/// Initialize the global memory manager
pub fn init_memory_manager(
    memory_map: &[impl petroleum::page_table::types::MemoryDescriptorValidator],
) -> SystemResult<()> {
    mem_debug!("Mem: init_memory_manager entered\n");

    let mut manager = MEMORY_MANAGER.lock();
    let mut memory_manager = UnifiedMemoryManager::new();

    if let Err(e) = memory_manager.init(memory_map) {
        mem_debug!("Mem: UnifiedMemoryManager::init failed!\n");
        return Err(e);
    }

    *manager = Some(memory_manager);
    mem_debug!("Mem: Global memory manager initialized\n");
    Ok(())
}

/// Get a reference to the global memory manager
pub fn get_memory_manager() -> &'static Mutex<Option<UnifiedMemoryManager>> {
    &MEMORY_MANAGER
}

/// Map a user page for kernel access
pub fn map_user_page(
    virtual_addr: usize,
    physical_addr: usize,
    flags: PageFlags,
) -> SystemResult<()> {
    if let Some(manager) = MEMORY_MANAGER.lock().as_mut() {
        manager.page_table_manager.map_page(
            virtual_addr,
            physical_addr,
            flags,
            petroleum::page_table::constants::get_frame_allocator_mut(),
        )
    } else {
        Err(SystemError::InternalError)
    }
}

// Re-export functions for easier access
pub use petroleum::{is_user_address, validate_user_buffer};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unified_memory_manager_creation() {
        let manager = UnifiedMemoryManager::new();
        assert_eq!(manager.name(), "UnifiedMemoryManager");
        assert_eq!(manager.priority(), 1000);
        assert!(!manager.is_initialized());
    }
}
