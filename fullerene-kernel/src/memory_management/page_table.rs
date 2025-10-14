//! Page Table Management Implementation
//!
//! This module provides page table operations and management using x86_64 structures.

// Import from parent module instead of crate root
use super::*;

// Import logging functions from crate namespace
use petroleum::common::logging as logging;

// Import needed types
use alloc::collections::BTreeMap;
use petroleum::page_table::{BootInfoFrameAllocator, EfiMemoryDescriptor};
use x86_64::{VirtAddr, PhysAddr, structures::paging::{PageTable, Page, PhysFrame, Mapper, FrameAllocator, Size4KiB, PageTableFlags as Flags, OffsetPageTable}};

/// A dummy frame allocator for when we need to allocate pages for page tables
pub struct DummyFrameAllocator {}

impl DummyFrameAllocator {
    pub fn new() -> Self {
        Self {}
    }
}

unsafe impl FrameAllocator<Size4KiB> for DummyFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        None // For now, we don't support allocating new frames for page tables
    }
}

/// Convert PageFlags to x86_64 PageTableFlags
fn convert_to_x86_64_flags(flags: crate::PageFlags) -> Flags {
    use x86_64::structures::paging::PageTableFlags as X86Flags;

    // Convert from our PageFlags to x86_64 flags
    let raw_flags = flags.as_u64();

    // Direct conversion assuming our flags match x86_64
    X86Flags::from_bits_truncate(raw_flags)
}

/// Process page table type alias for PageTableManager
pub type ProcessPageTable = PageTableManager;

/// Page table manager implementation
pub struct PageTableManager {
    current_page_table: usize,
    page_tables: BTreeMap<usize, usize>,
    initialized: bool,
    pub pml4_frame: crate::heap::PhysFrame,
}

/// Get the physical memory offset for virtual to physical address translation
pub fn get_physical_memory_offset() -> usize {
    use crate::memory_management::get_physical_memory_offset;
    get_physical_memory_offset()
}

impl PageTableManager {
    /// Create a new page table manager
    pub fn new() -> Self {
        Self {
            current_page_table: 0,
            page_tables: BTreeMap::new(),
            initialized: false,
            pml4_frame: crate::heap::PhysFrame::containing_address(x86_64::PhysAddr::new(0)),
        }
    }

    /// Initialize paging
    pub fn init_paging(&mut self) -> SystemResult<()> {
        // Get current CR3 (page table base)
        let (frame, _) = x86_64::registers::control::Cr3::read();
        self.current_page_table = frame.start_address().as_u64() as usize;
        self.pml4_frame = frame;

        self.initialized = true;
        // Logging disabled to avoid import issues
        // crate::logging::log_info("Page table manager initialized");
        Ok(())
    }

    /// Get the current page table
    fn get_current_page_table(&self) -> Option<&mut x86_64::structures::paging::PageTable> {
        use x86_64::structures::paging::PageTable;

        if !self.initialized {
            return None;
        }

        let phys_addr = self.current_page_table;
        // Use the physical memory offset to get the correct virtual address
        let virt_addr = crate::memory_management::physical_to_virtual(phys_addr) as *mut PageTable;
        Some(unsafe { &mut *virt_addr })
    }
}

// Implementation of PageTableHelper trait for PageTableManager
impl PageTableHelper for PageTableManager {
    fn map_page(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        flags: PageFlags,
    ) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        // Convert parameters to x86_64 types
        let virtual_addr = x86_64::VirtAddr::new(virtual_addr as u64);
        let physical_addr = x86_64::PhysAddr::new(physical_addr as u64);
        let page = x86_64::structures::paging::Page::<Size4KiB>::containing_address(virtual_addr);
        let frame = x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(physical_addr);
        let page_flags = convert_to_x86_64_flags(flags);

        // Get the active page table from CPU
        unsafe {
            let (current_level_4_table_frame, _) = x86_64::registers::control::Cr3::read();
            let current_table_phys = current_level_4_table_frame.start_address().as_u64() as *mut x86_64::structures::paging::PageTable;

            // Create mapper with physical memory offset
            let mut mapper = x86_64::structures::paging::OffsetPageTable::new(
                &mut *current_table_phys,
                x86_64::VirtAddr::new(get_physical_memory_offset() as u64),
            );

            // Map the page, creating intermediate tables if needed
            mapper.map_to(page, frame, page_flags, &mut DummyFrameAllocator::new())
                .map_err(|_| SystemError::MappingFailed)?
                .flush();
        }

        logging::log_info("Mapped page successfully");
        Ok(())
    }

    fn unmap_page(&mut self, _virtual_addr: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        log_info!("Unmapping virtual address");
        Ok(())
    }

    fn translate_address(&self, virtual_addr: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        // Use petroleum's translate_addr function
        let virt_addr = x86_64::VirtAddr::new(virtual_addr as u64);
        let offset = x86_64::VirtAddr::new(get_physical_memory_offset() as u64);

        if let Some(phys_addr) = unsafe { petroleum::page_table::translate_addr(virt_addr, offset) } {
            Ok(phys_addr.as_u64() as usize)
        } else {
            Err(SystemError::InvalidArgument)
        }
    }

    fn set_page_flags(&mut self, _virtual_addr: usize, _flags: PageFlags) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        log_info!("Setting page flags");
        Ok(())
    }

    fn get_page_flags(&self, _virtual_addr: usize) -> SystemResult<PageFlags> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        Ok(PageFlags::kernel_data())
    }

    fn flush_tlb(&mut self, _virtual_addr: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        // In a real implementation, this would flush the TLB
        log_info!("Flushing TLB for address");
        Ok(())
    }

    fn flush_tlb_all(&mut self) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        log_info!("Flushing entire TLB");
        Ok(())
    }

    fn create_page_table(&mut self) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        // In a real implementation, this would allocate a new page table
        let table_addr = 0x1000; // Dummy address
        self.page_tables.insert(table_addr, table_addr);
        Ok(table_addr)
    }

    fn destroy_page_table(&mut self, table_addr: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.page_tables.remove(&table_addr);
        Ok(())
    }

    fn clone_page_table(&mut self, source_table: usize) -> SystemResult<usize> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        let new_table = source_table + 0x1000; // Dummy offset
        self.page_tables.insert(new_table, source_table);
        Ok(new_table)
    }

    fn switch_page_table(&mut self, table_addr: usize) -> SystemResult<()> {
        if !self.initialized {
            return Err(SystemError::InternalError);
        }

        self.current_page_table = table_addr;
        log_info!("Switched page table");
        Ok(())
    }

    fn current_page_table(&self) -> usize {
        self.current_page_table
    }
}

// Implementation of Initializable trait for PageTableManager
impl Initializable for PageTableManager {
    fn init(&mut self) -> SystemResult<()> {
        self.init_paging()
    }

    fn name(&self) -> &'static str {
        "PageTableManager"
    }

    fn priority(&self) -> i32 {
        950 // Very high priority for page table management
    }
}

// Implementation of ErrorLogging trait for PageTableManager
impl ErrorLogging for PageTableManager {
    fn log_error(&self, error: &SystemError, context: &'static str) {
        log_error!(error, context);
    }

    fn log_warning(&self, message: &'static str) {
        log_warning!(message);
    }

    fn log_info(&self, message: &'static str) {
        log_info!(message);
    }

    fn log_debug(&self, message: &'static str) {
        log_debug!(message);
    }

    fn log_trace(&self, message: &'static str) {
        log_trace!(message);
    }
}
