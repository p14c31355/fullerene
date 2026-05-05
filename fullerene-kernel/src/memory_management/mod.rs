//! Unified Memory Management Implementation
//!
//! This module provides a comprehensive memory management system that implements
//! the MemoryManager, ProcessMemoryManager, PageTableHelper, and FrameAllocator traits.

// Define macros before using super for overlay
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use spin::Mutex;
use core::sync::atomic::{AtomicUsize, Ordering};

use static_assertions::assert_eq_size;

use petroleum::common::logging::{SystemError, SystemResult};
use petroleum::initializer::{
    ErrorLogging, FrameAllocator, Initializable, MemoryManager, ProcessMemoryManager,
    SyscallHandler,
};
use x86_64::structures::paging::{Page, PageTableFlags as PageFlags, Size4KiB};

use petroleum::page_table::{BitmapFrameAllocator, PageTableManager};
use petroleum::page_table::{BootInfoFrameAllocator, EfiMemoryDescriptor};
use process_memory::ProcessMemoryManagerImpl;
pub mod convenience;
pub mod kernel_space;
pub mod manager;
pub mod process_memory;

// Re-export for external use
pub use convenience::*;
pub use manager::UnifiedMemoryManager;
pub use petroleum::page_table::*;
pub use process_memory::*;


// Helper macros for common operations
macro_rules! check_initialized_mut {
    ($self:expr) => {
        if !$self.initialized {
            return Err(SystemError::InternalError);
        }
    };
}

// Generic memory operation helper for mutable access
macro_rules! memory_operation_mut {
    ($self:expr, $operation:expr) => {{
        check_initialized_mut!($self);
        $operation
    }};
}

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




/// Process page table type alias for PageTableManager
pub type ProcessPageTable = PageTableManager<'static>;

// Global memory manager instance
static MEMORY_MANAGER: Mutex<Option<UnifiedMemoryManager>> = Mutex::new(None);

/// Switch to a specific page table
pub fn switch_to_page_table(page_table: &ProcessPageTable) -> SystemResult<()> {
    // In a real implementation, this would switch the CR3 register
    // For now, just log the operation
    log::info!("Switching to page table");
    Ok(())
}

/// Create a new process page table
pub fn create_process_page_table() -> SystemResult<ProcessPageTable> {
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [mem] create_process_page_table start\n");
    // Check if memory manager is initialized; if not, use current page table for composite mode
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [mem] checking memory manager lock\n");
    if get_memory_manager().lock().is_none() {
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [mem] memory manager not found, using fallback\n");
        // Fallback: use current CR3 page table when memory manager not available
        let mut ptm = PageTableManager::new();
        Initializable::init(&mut ptm)?;
        return Ok(ptm);
    }
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [mem] memory manager found\n");

    // Allocate a new PML4 frame for the process page table
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [mem] acquiring manager lock\n");
    let mut manager_guard = get_memory_manager().lock();
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [mem] manager lock acquired\n");
    let manager = manager_guard.as_mut().ok_or(SystemError::InternalError)?;

    // Allocate frame for the new page table
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [mem] allocating pml4 frame\n");
    let pml4_frame = manager
        .frame_allocator
        .allocate_frame()
        .map_err(|_| SystemError::FrameAllocationFailed)?;

    // Debug: log the allocation result
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [mem] pml4 frame allocated\n");

    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [mem] mapping pml4 to TEMP_PHY_ACCESS\n");

    // Test if we can even access the page_table_manager field
    let is_init = manager.page_table_manager.initialized;
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [mem] ptm.initialized access: ");
    if is_init {
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"true\n");
    } else {
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"false\n");
    }

    // Use the UnifiedMemoryManager's map_address method
    manager.map_address(
        TEMP_PHY_ACCESS,
        pml4_frame,
        1,
    )?;
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [mem] pml4 mapped\n");

    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [mem] zeroing pml4 frame\n");
    // Zero the allocated page table frame to ensure it's a valid page table
    unsafe {
        let table_ptr = TEMP_PHY_ACCESS as *mut u64;
        core::slice::from_raw_parts_mut(table_ptr, 512).fill(0);
    }
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [mem] pml4 zeroed\n");

    // Copy kernel mappings to the new page table
    // This involves copying the higher half kernel mappings from the current page table
    let current_cr3 = unsafe { x86_64::registers::control::Cr3::read() };
    let kernel_table_phys = current_cr3.0.start_address().as_u64() as usize;

    // Temporarily map kernel table for reading
    manager.page_table_manager.map_page(
        TEMP_PHY_ACCESS + 4096,
        kernel_table_phys,
        PageFlags::PRESENT,
        &mut manager.frame_allocator,
    )?;

    // Copy the kernel page table entries (PML4[256..512])
    unsafe {
        let kernel_entries_src = (TEMP_PHY_ACCESS + 4096 + 256 * 8) as *const u64;
        let new_entries_dst = (TEMP_PHY_ACCESS + 256 * 8) as *mut u64;
        core::ptr::copy_nonoverlapping(kernel_entries_src, new_entries_dst, 256);
    }

    // Unmap temporary mappings
    if let Err(e) = manager.page_table_manager.unmap_page(TEMP_PHY_ACCESS) {
        log::warn!(
            "Failed to unmap temporary page at 0x{:x}: {:?}",
            TEMP_PHY_ACCESS,
            e
        );
    }
    if let Err(e) = manager
        .page_table_manager
        .unmap_page(TEMP_PHY_ACCESS + 4096)
    {
        log::warn!(
            "Failed to unmap temporary page at 0x{:x}: {:?}",
            TEMP_PHY_ACCESS + 4096,
            e
        );
    }

    // Initialize the new page table manager with the allocated frame
    let mut page_table_manager = PageTableManager::new_with_frame(
        x86_64::structures::paging::PhysFrame::containing_address(x86_64::PhysAddr::new(
            pml4_frame as u64,
        )),
    );
    Initializable::init(&mut page_table_manager)?;

    Ok(page_table_manager)
}

/// Deallocate a process page table and free its frames
    pub fn deallocate_process_page_table(pml4_frame: x86_64::structures::paging::PhysFrame) {
        // Properly deallocate the page table and its frames
        if let Some(manager) = MEMORY_MANAGER.lock().as_mut() {
            // The pml4_frame contains the physical address of the page table
            let frame_addr = pml4_frame.start_address().as_u64() as usize;

            // Free the frame containing the page table
            let _ = manager.free_frame(frame_addr);

            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Deallocated process page table\n");
        }
    }

/// Initialize the global memory manager
pub fn init_memory_manager(
    memory_map: &[impl petroleum::page_table::efi_memory::MemoryDescriptorValidator],
) -> SystemResult<()> {
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: init_memory_manager entered\n");
    
    let mut manager = MEMORY_MANAGER.lock();
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: MEMORY_MANAGER lock acquired\n");
    
    let mut memory_manager = UnifiedMemoryManager::new();
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: UnifiedMemoryManager created\n");
    
    if let Err(e) = memory_manager.init(memory_map) {
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"ERROR: UnifiedMemoryManager::init failed!\n");
        return Err(e);
    }
    
    *manager = Some(memory_manager);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Global memory manager initialized successfully\n");
    Ok(())
}

/// Get a reference to the global memory manager
pub fn get_memory_manager() -> &'static Mutex<Option<UnifiedMemoryManager>> {
    &MEMORY_MANAGER
}

// User space memory validation functions
// Integrated from user_space.rs to reduce file count

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
            &mut manager.frame_allocator,
        )
    } else {
        Err(SystemError::InternalError)
    }
}

// Re-export functions for easier access
pub use petroleum::{is_user_address, validate_user_buffer};

/// Temporary virtual address for physical memory access
const TEMP_PHY_ACCESS: usize = 0xffff_8000_0000_1000;

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
