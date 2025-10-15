use alloc::collections::BTreeMap;
use petroleum::common::logging::{SystemError, SystemResult};

use super::*;

/// Process-specific memory manager implementation
pub struct ProcessMemoryManagerImpl {
    process_id: usize,
    page_table_root: usize,
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
            page_table_root: 0,
            heap_start: 0x4000_0000, // Start heap at 1GB
            heap_end: 0x4000_0000,
            stack_start: 0x7FFF_0000, // Start stack near top of user space
            stack_end: 0x7FFF_0000,
            allocations: BTreeMap::new(),
        }
    }

    /// Get the page table root address
    pub fn page_table_root(&self) -> usize {
        self.page_table_root
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
