use alloc::collections::BTreeMap;
use petroleum::common::logging::{SystemError, SystemResult};
use petroleum::page_table::PageTableHelper;
use petroleum::page_table::process::ProcessPageTable;

/// Process-specific memory manager implementation
pub struct ProcessMemoryManagerImpl {
    process_id: usize,
    page_table: ProcessPageTable,
    heap_start: usize,
    heap_end: usize,
    stack_start: usize,
    stack_end: usize,
    allocations: BTreeMap<usize, usize>, // address -> size mapping
}

use crate::*;

impl ProcessMemoryManagerImpl {
    /// Create a new process memory manager
    pub fn new(process_id: usize) -> Self {
        Self {
            process_id,
            page_table: ProcessPageTable::new(),
            heap_start: 0x4000_0000, // Start heap at 1GB
            heap_end: 0x4000_0000,
            stack_start: 0x7FFF_0000, // Start stack near top of user space
            stack_end: 0x7FFF_0000,
            allocations: BTreeMap::new(),
        }
    }

    /// Initialize the process page table by cloning the kernel page table
    pub fn init_page_table(
        &mut self,
        pt_manager: &mut petroleum::page_table::process::ProcessPageTable,
        frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<x86_64::structures::paging::Size4KiB>,
    ) -> SystemResult<()> {
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_page_table] entered\n");
        let kernel_root = pt_manager.current_page_table();
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_page_table] calling clone_page_table\n");
        let new_root = pt_manager.clone_page_table(kernel_root, frame_allocator)?;
        // Don't switch CR3 - just store the new root for later context switch.
        // The CR3 switch would require the new root's frame to be in our own
        // allocated_tables map, which it's not (it's in pt_manager's map).
        if let Some(&frame) = pt_manager.allocated_tables().get(&new_root) {
            self.page_table.allocated_tables_mut().insert(new_root, frame);
            self.page_table.set_current(new_root);
        }
        Ok(())
    }

    /// Get the page table root address
    pub fn page_table_root(&self) -> usize {
        self.page_table.current_page_table()
    }

    /// Allocate memory from heap
    pub fn allocate_heap(&mut self, size: usize) -> SystemResult<usize> {
        let aligned_size = (size + 4095) & !(4095); // Page align
        let address = self.heap_end;

        self.allocations.insert(address, aligned_size);
        self.heap_end += aligned_size;

        Ok(address)
    }

    /// Free memory from heap
    pub fn free_heap(&mut self, address: usize, size: usize) -> SystemResult<()> {
        if let Some(&alloc_size) = self.allocations.get(&address) {
            if alloc_size == size {
                self.allocations.remove(&address);
                return Ok(());
            }
        }

        Err(SystemError::InvalidArgument)
    }

    /// Allocate memory from stack
    pub fn allocate_stack(&mut self, size: usize) -> SystemResult<usize> {
        let aligned_size = (size + 4095) & !(4095); // Page align

        if self.stack_start < aligned_size {
            return Err(SystemError::MemOutOfMemory);
        }

        self.stack_start -= aligned_size;
        let address = self.stack_start;

        self.allocations.insert(address, aligned_size);

        Ok(address)
    }

    /// Free memory from stack
    pub fn free_stack(&mut self, address: usize, size: usize) -> SystemResult<()> {
        if let Some(&alloc_size) = self.allocations.get(&address) {
            if alloc_size == size {
                self.allocations.remove(&address);
                return Ok(());
            }
        }

        Err(SystemError::InvalidArgument)
    }

    /// Cleanup process memory
    pub fn cleanup(&mut self) -> SystemResult<()> {
        self.allocations.clear();
        log::info!("Process memory cleaned up");
        Ok(())
    }
}
