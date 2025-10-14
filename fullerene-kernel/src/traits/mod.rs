//! Core system traits
//!
//! This module defines the core traits used throughout the Fullerene kernel system.

use crate::{SystemError, SystemResult, PageFlags};

// Placeholder for hardware-related traits that may need to be defined
pub trait HardwareDevice: Initializable + ErrorLogging {
    fn device_name(&self) -> &'static str;
    fn device_type(&self) -> &'static str;
    fn enable(&mut self) -> SystemResult<()>;
    fn disable(&mut self) -> SystemResult<()>;
    fn reset(&mut self) -> SystemResult<()>;
    fn is_enabled(&self) -> bool;
    fn read(&mut self, address: usize, buffer: &mut [u8]) -> SystemResult<usize> {
        // Default implementation returns error - hardware-specific devices should override
        Err(SystemError::NotSupported)
    }
    fn write(&mut self, address: usize, buffer: &[u8]) -> SystemResult<usize> {
        // Default implementation returns error - hardware-specific devices should override
        Err(SystemError::NotSupported)
    }
}

// Placeholder for syscall-related traits that may need to be defined
pub trait SyscallHandler {
    fn handle_syscall(&mut self, syscall_number: usize, args: &[usize]) -> SystemResult<usize>;
}

// Placeholder for logging-related traits that may need to be defined
pub trait Logger {
    fn log(&self, level: crate::LogLevel, message: &str);
}

// Core system traits
pub trait Initializable {
    fn init(&mut self) -> SystemResult<()>;
    fn name(&self) -> &'static str;
    fn priority(&self) -> i32;
}

pub trait ErrorLogging {
    fn log_error(&self, error: &SystemError, context: &'static str);
    fn log_warning(&self, message: &'static str);
    fn log_info(&self, message: &'static str);
}

pub trait MemoryManager {
    fn allocate_pages(&mut self, count: usize) -> SystemResult<usize>;
    fn free_pages(&mut self, address: usize, count: usize) -> SystemResult<()>;
    fn total_memory(&self) -> usize;
    fn available_memory(&self) -> usize;
    fn map_address(&mut self, virtual_addr: usize, physical_addr: usize, count: usize) -> SystemResult<()>;
    fn unmap_address(&mut self, virtual_addr: usize, count: usize) -> SystemResult<()>;
    fn virtual_to_physical(&self, virtual_addr: usize) -> SystemResult<usize>;
    fn init_paging(&mut self) -> SystemResult<()>;
    fn page_size(&self) -> usize;
}

pub trait ProcessMemoryManager {
    fn create_address_space(&mut self, process_id: usize) -> SystemResult<()>;
    fn switch_address_space(&mut self, process_id: usize) -> SystemResult<()>;
    fn destroy_address_space(&mut self, process_id: usize) -> SystemResult<()>;
    fn allocate_heap(&mut self, size: usize) -> SystemResult<usize>;
    fn free_heap(&mut self, address: usize, size: usize) -> SystemResult<()>;
    fn allocate_stack(&mut self, size: usize) -> SystemResult<usize>;
    fn free_stack(&mut self, address: usize, size: usize) -> SystemResult<()>;
    fn copy_memory_between_processes(&mut self, from_process: usize, to_process: usize, from_addr: usize, to_addr: usize, size: usize) -> SystemResult<()>;
    fn current_process_id(&self) -> usize;
}

pub trait PageTableHelper {
    fn map_page(&mut self, virtual_addr: usize, physical_addr: usize, flags: PageFlags) -> SystemResult<()>;
    fn unmap_page(&mut self, virtual_addr: usize) -> SystemResult<()>;
    fn translate_address(&self, virtual_addr: usize) -> SystemResult<usize>;
    fn set_page_flags(&mut self, virtual_addr: usize, flags: PageFlags) -> SystemResult<()>;
    fn get_page_flags(&self, virtual_addr: usize) -> SystemResult<PageFlags>;
    fn flush_tlb(&mut self, virtual_addr: usize) -> SystemResult<()>;
    fn flush_tlb_all(&mut self) -> SystemResult<()>;
    fn create_page_table(&mut self) -> SystemResult<usize>;
    fn destroy_page_table(&mut self, table_addr: usize) -> SystemResult<()>;
    fn clone_page_table(&mut self, source_table: usize) -> SystemResult<usize>;
    fn switch_page_table(&mut self, table_addr: usize) -> SystemResult<()>;
    fn current_page_table(&self) -> usize;
}

pub trait FrameAllocator {
    fn allocate_frame(&mut self) -> SystemResult<usize>;
    fn free_frame(&mut self, frame_addr: usize) -> SystemResult<()>;
    fn allocate_contiguous_frames(&mut self, count: usize) -> SystemResult<usize>;
    fn free_contiguous_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()>;
    fn total_frames(&self) -> usize;
    fn available_frames(&self) -> usize;
    fn reserve_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()>;
    fn release_frames(&mut self, start_addr: usize, count: usize) -> SystemResult<()>;
    fn is_frame_available(&self, frame_addr: usize) -> bool;
    fn frame_size(&self) -> usize;
}

// Additional trait definitions can be added here if needed, but avoid re-exports that cause conflicts

// Additional trait implementations can be added here if needed
