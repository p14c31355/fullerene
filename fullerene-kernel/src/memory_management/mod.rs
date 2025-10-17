//! Unified Memory Management Implementation
//!
//! This module provides a comprehensive memory management system that implements
//! the MemoryManager, ProcessMemoryManager, PageTableHelper, and FrameAllocator traits.

// Define macros before using super for overlay
use alloc::collections::BTreeMap;
use spin::Mutex;

use static_assertions::assert_eq_size;

use crate::traits::{
    ErrorLogging, FrameAllocator, Initializable, MemoryManager, PageTableHelper,
    ProcessMemoryManager,
};
use petroleum::common::logging::{SystemError, SystemResult};
use x86_64::structures::paging::{PageTableFlags as PageFlags, Size4KiB};

use frame_allocator::BitmapFrameAllocator;
use page_table::PageTableManager;
use petroleum::page_table::{BootInfoFrameAllocator, EfiMemoryDescriptor};
use process_memory::ProcessMemoryManagerImpl;
pub mod convenience;
pub mod frame_allocator;
pub mod page_table;
pub mod process_memory;
pub mod user_space;

// Re-export for external use
pub use convenience::*;
pub use frame_allocator::*;
pub use petroleum::page_table::*;
pub use process_memory::*;
pub use user_space::*;

#[macro_export]
macro_rules! align_page {
    ($size:expr) => {{
        const PAGE_SIZE: usize = 4096;
        ($size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
    }};
}

// Helper macros for common operations
macro_rules! check_initialized {
    ($self:expr) => {
        if !$self.initialized {
            return Err(SystemError::InternalError);
        }
    };
}

macro_rules! check_initialized_mut {
    ($self:expr) => {
        if !$self.initialized {
            return Err(SystemError::InternalError);
        }
    };
}

macro_rules! with_memory_manager {
    ($manager:expr, $operation:expr) => {
        if let Some(manager) = $manager {
            $operation
        } else {
            Err(SystemError::InternalError)
        }
    };
}

// Generic memory operation helper
macro_rules! memory_operation {
    ($self:expr, $operation:expr) => {{
        check_initialized!($self);
        $operation
    }};
}

// Generic memory operation helper for mutable access
macro_rules! memory_operation_mut {
    ($self:expr, $operation:expr) => {{
        check_initialized_mut!($self);
        $operation
    }};
}

// Generic helper for looping over pages
macro_rules! for_each_page {
    ($start:expr, $count:expr, $body:expr) => {
        for i in 0..$count {
            let addr = $start + (i * 4096);
            $body(addr, i);
        }
    };
}

// Generic helper for frame allocations
macro_rules! with_current_process_manager {
    ($self:expr, $operation:expr) => {
        if let Some(process_manager) = $self.process_managers.get_mut(&$self.current_process) {
            $operation(process_manager)
        } else {
            Err(SystemError::NoSuchProcess)
        }
    };
}

// Memory management error types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    MappingFailed,
    UnmappingFailed,
    FrameAllocationFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocError {
    OutOfMemory,
    MappingFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreeError {
    UnmappingFailed,
}

/// Unified memory manager implementing all memory management traits
pub struct UnifiedMemoryManager {
    frame_allocator: BitmapFrameAllocator,
    page_table_manager: PageTableManager,
    process_managers: BTreeMap<usize, ProcessMemoryManagerImpl>,
    current_process: usize,
    initialized: bool,
}

impl UnifiedMemoryManager {
    /// Create a new unified memory manager
    pub fn new() -> Self {
        Self {
            frame_allocator: BitmapFrameAllocator::new(),
            page_table_manager: PageTableManager::new(),
            process_managers: BTreeMap::new(),
            current_process: 0,
            initialized: false,
        }
    }

    /// Initialize the memory management system
    pub fn init(
        &mut self,
        memory_map: &'static [petroleum::page_table::EfiMemoryDescriptor],
    ) -> SystemResult<()> {
        // Initialize frame allocator with memory map
        self.frame_allocator.init_with_memory_map(memory_map)?;

        // Allocate a frame for the page table manager
        let pml4_frame_addr = self.frame_allocator.allocate_frame()
            .map_err(|_| petroleum::common::logging::SystemError::FrameAllocationFailed)?;
        let pml4_frame = x86_64::structures::paging::PhysFrame::containing_address(
            x86_64::PhysAddr::new(pml4_frame_addr as u64)
        );

        log::info!("PageTableManager: Allocated frame at physical address: 0x{:x}", pml4_frame_addr);

        // For now, assume the frame allocator returns zeroed frames
        // TODO: Properly zero the frame through virtual memory mapping

        // Set the frame on page table manager
        self.page_table_manager.set_pml4_frame(pml4_frame);

        // Initialize page table manager
        Initializable::init(&mut self.page_table_manager)?;

        // Create kernel address space (process 0)
        self.create_address_space(0)?;

        self.initialized = true;
        log::info!("Unified memory manager initialized");
        Ok(())
    }

    /// Get frame allocator reference
    pub fn frame_allocator(&self) -> &BitmapFrameAllocator {
        &self.frame_allocator
    }

    /// Get frame allocator mutable reference
    pub fn frame_allocator_mut(&mut self) -> &mut BitmapFrameAllocator {
        &mut self.frame_allocator
    }

    /// Get page table manager reference
    pub fn page_table_manager(&self) -> &PageTableManager {
        &self.page_table_manager
    }

    /// Get page table manager mutable reference
    pub fn page_table_manager_mut(&mut self) -> &mut PageTableManager {
        &mut self.page_table_manager
    }

    /// Check if memory manager is initialized
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }
}

// Implementation of base MemoryManager trait
impl MemoryManager for UnifiedMemoryManager {
    fn allocate_pages(&mut self, count: usize) -> SystemResult<usize> {
        memory_operation_mut!(self, {
            // Allocate physical frames
            let frame_addr = self.frame_allocator.allocate_contiguous_frames(count)?;

            // Map to kernel virtual address space
            let virtual_addr = self.find_free_virtual_address(count * 4096)?;

            for i in 0..count {
                let phys_addr = frame_addr + (i * 4096);
                let virt_addr = virtual_addr + (i * 4096);
                self.page_table_manager.map_page(
                    virt_addr,
                    phys_addr,
                    PageFlags::PRESENT | PageFlags::WRITABLE,
                    &mut self.frame_allocator,
                )?;
            }

            Ok(virtual_addr)
        })
    }

    fn free_pages(&mut self, address: usize, count: usize) -> SystemResult<()> {
        memory_operation_mut!(self, {
            // Get physical addresses and free frames
            for i in 0..count {
                let virt_addr = address + (i * 4096);
                if let Ok(phys_addr) = self.page_table_manager.translate_address(virt_addr) {
                    self.frame_allocator.free_frame(phys_addr)?;
                }
                self.page_table_manager.unmap_page(virt_addr)?;
            }

            Ok(())
        })
    }

    fn total_memory(&self) -> usize {
        self.frame_allocator.total_frames() * self.frame_allocator.frame_size()
    }

    fn available_memory(&self) -> usize {
        self.frame_allocator.available_frames() * self.frame_allocator.frame_size()
    }

    fn map_address(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        count: usize,
    ) -> SystemResult<()> {
        memory_operation_mut!(self, {
            for i in 0..count {
                let vaddr = virtual_addr + (i * 4096);
                let paddr = physical_addr + (i * 4096);
                self.page_table_manager.map_page(
                    vaddr,
                    paddr,
                    PageFlags::PRESENT | PageFlags::WRITABLE,
                    &mut self.frame_allocator,
                )?;
            }
            Ok(())
        })
    }

    fn unmap_address(&mut self, virtual_addr: usize, count: usize) -> SystemResult<()> {
        memory_operation_mut!(self, {
            for i in 0..count {
                let vaddr = virtual_addr + (i * 4096);
                self.page_table_manager.unmap_page(vaddr)?;
            }
            Ok(())
        })
    }

    fn virtual_to_physical(&self, virtual_addr: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.page_table_manager.translate_address(virtual_addr)
    }

    fn init_paging(&mut self) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.page_table_manager.init_paging()
    }

    fn page_size(&self) -> usize {
        4096
    }
}

// Implementation of ProcessMemoryManager trait
impl ProcessMemoryManager for UnifiedMemoryManager {
    fn create_address_space(&mut self, process_id: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        let process_manager = ProcessMemoryManagerImpl::new(process_id);
        self.process_managers.insert(process_id, process_manager);

        log::info!("Created address space for process");
        Ok(())
    }

    fn switch_address_space(&mut self, process_id: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        if let Some(process_manager) = self.process_managers.get(&process_id) {
            self.current_process = process_id;
            self.page_table_manager
                .switch_page_table(process_manager.page_table_root())?;
            log::info!("Switched to process address space");
            Ok(())
        } else {
            Err(SystemError::NoSuchProcess)
        }
    }

    fn destroy_address_space(&mut self, process_id: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        if let Some(mut process_manager) = self.process_managers.remove(&process_id) {
            process_manager.cleanup()?;
            log::info!("Destroyed address space for process");
            Ok(())
        } else {
            Err(SystemError::NoSuchProcess)
        }
    }

    fn allocate_heap(&mut self, size: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        if let Some(process_manager) = self.process_managers.get_mut(&self.current_process) {
            process_manager.allocate_heap(size)
        } else {
            Err(SystemError::NoSuchProcess)
        }
    }

    fn free_heap(&mut self, address: usize, size: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        if let Some(process_manager) = self.process_managers.get_mut(&self.current_process) {
            process_manager.free_heap(address, size)
        } else {
            Err(SystemError::NoSuchProcess)
        }
    }

    fn allocate_stack(&mut self, size: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        if let Some(process_manager) = self.process_managers.get_mut(&self.current_process) {
            process_manager.allocate_stack(size)
        } else {
            Err(SystemError::NoSuchProcess)
        }
    }

    fn free_stack(&mut self, address: usize, size: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        if let Some(process_manager) = self.process_managers.get_mut(&self.current_process) {
            process_manager.free_stack(address, size)
        } else {
            Err(SystemError::NoSuchProcess)
        }
    }

    fn copy_memory_between_processes(
        &mut self,
        from_process: usize,
        to_process: usize,
        from_addr: usize,
        to_addr: usize,
        size: usize,
    ) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        // For simplicity, implement as copy through kernel space
        // In a full implementation, this would use proper inter-process memory copying

        // Save current process
        let current_process = self.current_process;

        // Copy from source process
        self.switch_address_space(from_process)?;
        let source_data = self.copy_from_user_space(from_addr, size)?;

        // Copy to destination process
        self.switch_address_space(to_process)?;
        self.copy_to_user_space(to_addr, &source_data)?;

        // Restore original process
        self.switch_address_space(current_process)?;

        Ok(())
    }

    fn current_process_id(&self) -> usize {
        self.current_process
    }
}

// Implementation of PageTableHelper trait
impl PageTableHelper for UnifiedMemoryManager {
    fn map_page(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        flags: PageFlags,
        frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
    ) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.page_table_manager
            .map_page(virtual_addr, physical_addr, flags, frame_allocator)
    }

    fn unmap_page(&mut self, virtual_addr: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.page_table_manager.unmap_page(virtual_addr)
    }

    fn translate_address(&self, virtual_addr: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.page_table_manager.translate_address(virtual_addr)
    }

    fn set_page_flags(&mut self, virtual_addr: usize, flags: PageFlags) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.page_table_manager.set_page_flags(virtual_addr, flags)
    }

    fn get_page_flags(&self, virtual_addr: usize) -> SystemResult<PageFlags> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.page_table_manager.get_page_flags(virtual_addr)
    }

    fn flush_tlb(&mut self, virtual_addr: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.page_table_manager.flush_tlb(virtual_addr)
    }

    fn flush_tlb_all(&mut self) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.page_table_manager.flush_tlb_all()
    }

    fn create_page_table(&mut self) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.page_table_manager.create_page_table()
    }

    fn destroy_page_table(&mut self, table_addr: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.page_table_manager.destroy_page_table(table_addr)
    }

    fn clone_page_table(&mut self, source_table: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.page_table_manager.clone_page_table(source_table)
    }

    fn switch_page_table(&mut self, table_addr: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.page_table_manager.switch_page_table(table_addr)
    }

    fn current_page_table(&self) -> usize {
        self.page_table_manager.current_page_table()
    }
}

// Implementation of FrameAllocator trait
impl FrameAllocator for UnifiedMemoryManager {
    fn allocate_frame(&mut self) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.frame_allocator.allocate_frame()
    }

    fn free_frame(&mut self, frame_addr: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.frame_allocator.free_frame(frame_addr)
    }

    fn allocate_contiguous_frames(&mut self, count: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.frame_allocator.allocate_contiguous_frames(count)
    }

    fn free_contiguous_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.frame_allocator
            .free_contiguous_frames(start_addr, count)
    }

    fn total_frames(&self) -> usize {
        self.frame_allocator.total_frames()
    }

    fn available_frames(&self) -> usize {
        self.frame_allocator.available_frames()
    }

    fn reserve_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.frame_allocator.reserve_frames(start_addr, count)
    }

    fn release_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.frame_allocator.release_frames(start_addr, count)
    }

    fn is_frame_available(&self, frame_addr: usize) -> bool {
        self.frame_allocator.is_frame_available(frame_addr)
    }

    fn frame_size(&self) -> usize {
        self.frame_allocator.frame_size()
    }
}

// Implementation of Initializable trait
impl Initializable for UnifiedMemoryManager {
    fn init(&mut self) -> SystemResult<()> {
        // Initialize with a dummy memory map for now
        // In a real implementation, this would be called with the actual EFI memory map
        let dummy_memory_map = &[];
        self.init(dummy_memory_map)
    }

    fn name(&self) -> &'static str {
        "UnifiedMemoryManager"
    }

    fn priority(&self) -> i32 {
        1000 // Highest priority for memory management
    }
}

// Implementation of ErrorLogging trait
impl ErrorLogging for UnifiedMemoryManager {
    fn log_error(&self, error: &SystemError, context: &'static str) {
        log::error!("SystemError({}): {}", *error as u32, context);
    }

    fn log_warning(&self, message: &'static str) {
        log::warn!("{}", message);
    }

    fn log_info(&self, message: &'static str) {
        log::info!("{}", message);
    }

    fn log_debug(&self, message: &'static str) {
        log::debug!("{}", message);
    }

    fn log_trace(&self, message: &'static str) {
        log::trace!("{}", message);
    }
}

// Helper methods for UnifiedMemoryManager
impl UnifiedMemoryManager {
    fn find_free_virtual_address(&self, size: usize) -> SystemResult<usize> {
        // TODO: Implement a proper kernel virtual address space allocator.
        // For now, using a simple bump allocator starting from kernel space base

        // Use a static counter as a simple bump allocator
        static mut KERNEL_VIRTUAL_BUMP: usize = 0xFFFF_8000_0000_0000;

        unsafe {
            let addr = KERNEL_VIRTUAL_BUMP;
            KERNEL_VIRTUAL_BUMP += size;
            Ok(addr)
        }
    }

    /// Copy data from user space to kernel space
    fn copy_from_user_space(
        &self,
        user_addr: usize,
        size: usize,
    ) -> SystemResult<alloc::vec::Vec<u8>> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        let mut data = alloc::vec::Vec::with_capacity(size);

        for offset in (0..size).step_by(4096) {
            let page_size = core::cmp::min(4096, size - offset);
            let virt_addr = user_addr + offset;

            if let Ok(phys_addr) = self.page_table_manager.translate_address(virt_addr) {
                // Convert physical address to virtual address using the offset
                let virtual_phys_addr = physical_to_virtual(phys_addr) + (offset % 4096);
                unsafe {
                    let phys_ptr = virtual_phys_addr as *const u8;
                    let slice = core::slice::from_raw_parts(phys_ptr, page_size);
                    data.extend_from_slice(slice);
                }
            } else {
                return Err(SystemError::InvalidArgument);
            }
        }

        Ok(data)
    }

    /// Copy data from kernel space to user space
    fn copy_to_user_space(&mut self, user_addr: usize, data: &[u8]) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        for (i, chunk) in data.chunks(4096).enumerate() {
            let offset = i * 4096;
            let virt_addr = user_addr + offset;

            // Ensure page is mapped by allocating if necessary
            if self
                .page_table_manager
                .translate_address(virt_addr)
                .is_err()
            {
                let frame = self.frame_allocator.allocate_frame()?;
                self.page_table_manager.map_page(
                    virt_addr,
                    frame,
                    PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER_ACCESSIBLE,
                    &mut self.frame_allocator,
                )?;
            }

            if let Ok(phys_addr) = self.page_table_manager.translate_address(virt_addr) {
                // Convert physical address to virtual address using the offset
                let virtual_phys_addr = physical_to_virtual(phys_addr) + (offset % 4096);
                unsafe {
                    let phys_ptr = virtual_phys_addr as *mut u8;
                    core::ptr::copy_nonoverlapping(chunk.as_ptr(), phys_ptr, chunk.len());
                }
            } else {
                return Err(SystemError::InvalidArgument);
            }
        }

        Ok(())
    }
}

/// Process page table type alias for PageTableManager
pub type ProcessPageTable = PageTableManager;

// Global memory manager instance
static MEMORY_MANAGER: Mutex<Option<UnifiedMemoryManager>> = Mutex::new(None);

/// Physical memory offset for virtual to physical address translation
pub const PHYSICAL_MEMORY_OFFSET_BASE: usize = 0xFFFF_8000_0000_0000;

/// Switch to a specific page table
pub fn switch_to_page_table(page_table: &ProcessPageTable) -> SystemResult<()> {
    // In a real implementation, this would switch the CR3 register
    // For now, just log the operation
    log::info!("Switching to page table");
    Ok(())
}

/// Create a new process page table
pub fn create_process_page_table() -> SystemResult<ProcessPageTable> {
    // Allocate a new PML4 frame for the process page table
    let mut manager_guard = get_memory_manager().lock();
    let manager = manager_guard.as_mut().ok_or(SystemError::InternalError)?;

    // Allocate frame for the new page table
    let pml4_frame = manager
        .frame_allocator
        .allocate_frame()
        .map_err(|_| SystemError::FrameAllocationFailed)?;

    // Zero the allocated page table frame to ensure it's a valid page table
    let new_table_virt = physical_to_virtual(pml4_frame) as *mut u8;
    unsafe {
        core::ptr::write_bytes(new_table_virt, 0, 4096);
    }

    // Copy kernel mappings to the new page table
    // This involves copying the higher half kernel mappings from the current page table
    let current_cr3 = unsafe { x86_64::registers::control::Cr3::read() };
    let kernel_table_phys = current_cr3.0.start_address().as_u64();

    // Map the new page table temporarily to copy kernel mappings
    let kernel_table_virt = physical_to_virtual(kernel_table_phys.try_into().unwrap());
    let new_table_virt_u64 = new_table_virt as u64;

    // Copy the kernel page table entries (PML4[256..512])
    unsafe {
        core::ptr::copy_nonoverlapping(
            (kernel_table_virt + 256 * 8) as *const u64,
            (new_table_virt_u64 + 256 * 8) as *mut u64,
            256,
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

        log::info!("Deallocated process page table");
    }
}

/// Initialize the global memory manager
pub fn init_memory_manager(
    memory_map: &'static [petroleum::page_table::EfiMemoryDescriptor],
) -> SystemResult<()> {
    let mut manager = MEMORY_MANAGER.lock();
    let mut memory_manager = UnifiedMemoryManager::new();
    memory_manager.init(memory_map)?;
    *manager = Some(memory_manager);

    log::info!("Global memory manager initialized");
    Ok(())
}

/// Get a reference to the global memory manager
pub fn get_memory_manager() -> &'static Mutex<Option<UnifiedMemoryManager>> {
    &MEMORY_MANAGER
}

// Re-export functions for easier access
pub use user_space::{is_user_address, map_user_page};

/// Physical memory offset for virtual to physical address translation
static PHYSICAL_MEMORY_OFFSET: spin::Mutex<usize> = spin::Mutex::new(0);

/// Set the physical memory offset for virtual to physical address translation
pub fn set_physical_memory_offset(offset: usize) {
    *PHYSICAL_MEMORY_OFFSET.lock() = offset;
}

/// Get the physical memory offset for virtual to physical address translation
pub fn get_physical_memory_offset() -> usize {
    *PHYSICAL_MEMORY_OFFSET.lock()
}

/// Convert virtual address to physical address using the offset
pub fn virtual_to_physical(virtual_addr: usize) -> usize {
    virtual_addr - get_physical_memory_offset()
}

/// Convert physical address to virtual address using the offset
pub fn physical_to_virtual(physical_addr: usize) -> usize {
    physical_addr + get_physical_memory_offset()
}

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
