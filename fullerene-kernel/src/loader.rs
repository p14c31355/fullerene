//! Program loader for Fullerene OS
//!
//! This module is responsible for loading executable programs into memory
//! and creating processes to run them.

#![no_std]

use crate::process;
use alloc::vec::Vec;
use core::ptr;
use x86_64::{PhysAddr, VirtAddr};

pub const PROGRAM_LOAD_BASE: u64 = 0x400000; // 4MB base address for user programs

/// Simple ELF header structure (simplified)
#[repr(C)]
#[derive(Debug)]
struct ElfHeader {
    magic: [u8; 4],
    class: u8,
    endianness: u8,
    version: u8,
    abi: u8,
    abi_version: u8,
    _pad: [u8; 7],
    elf_type: u16,
    machine: u16,
    elf_version: u32,
    entry_point: u64,
    program_header_offset: u64,
    section_header_offset: u64,
    flags: u32,
    header_size: u16,
    program_header_entry_size: u16,
    program_header_count: u16,
    section_header_entry_size: u16,
    section_header_count: u16,
    section_name_index: u16,
}

#[repr(C)]
#[derive(Debug)]
struct ProgramHeader {
    p_type: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    paddr: u64,
    file_size: u64,
    mem_size: u64,
    align: u64,
}

// ELF constants
const ELFMAG: [u8; 4] = [0x7F, b'E', b'L', b'F'];
const PT_LOAD: u32 = 1;

/// Load a program from raw bytes and create a process for it
pub fn load_program(
    image_data: &[u8],
    name: &'static str,
) -> Result<process::ProcessId, LoadError> {
    // Parse ELF header
    if image_data.len() < core::mem::size_of::<ElfHeader>() {
        return Err(LoadError::InvalidFormat);
    }

    let elf_header = unsafe { &*(image_data.as_ptr() as *const ElfHeader) };

    // Verify ELF magic
    if elf_header.magic != ELFMAG {
        return Err(LoadError::InvalidFormat);
    }

    // Verify this is an executable
    if elf_header.elf_type != 2 {
        // ET_EXEC
        return Err(LoadError::NotExecutable);
    }

    // Calculate program headers location
    let ph_offset = elf_header.program_header_offset as usize;
    let ph_count = elf_header.program_header_count as usize;
    let ph_entry_size = elf_header.program_header_entry_size as usize;

    // Find entry point
    let entry_point: fn() = unsafe { core::mem::transmute(elf_header.entry_point as usize) };

    // Create process with the loaded program
    let pid = process::create_process(name, entry_point);

    // Get the process's page table (assume it's created in create_process)
    // For now, we skip loading segments due to page table integration not implemented yet
    /*
    let process_page_table = &mut process_list.iter_mut().find(|p| p.id == pid).unwrap().page_table.as_mut().unwrap();

    // Load program segments
    for i in 0..ph_count {
        let ph_offset = ph_offset + i * ph_entry_size;
        if ph_offset + core::mem::size_of::<ProgramHeader>() > image_data.len() {
            return Err(LoadError::InvalidFormat);
        }

        let ph = unsafe { &*(image_data.as_ptr().add(ph_offset) as *const ProgramHeader) };

        // Only load PT_LOAD segments
        if ph.p_type == PT_LOAD {
            load_segment(ph, image_data, process_page_table)?;
        }
    }
    */

    // TODO: Load segments after page table integration

    Ok(pid)
}

/// Load a program segment into memory
fn load_segment(ph: &ProgramHeader, image_data: &[u8], page_table: &mut crate::memory_management::ProcessPageTable) -> Result<(), LoadError> {
    let file_offset = ph.offset as usize;
    let file_size = ph.file_size as usize;
    let mem_size = ph.mem_size as usize;
    let vaddr = ph.vaddr as usize;

    // Bounds check
    if file_offset + file_size > image_data.len() {
        return Err(LoadError::InvalidFormat);
    }

    // Validate that the virtual address range is in user space and no overflow
    use crate::memory_management::{is_user_address, allocate_user_memory};
    use x86_64::VirtAddr;

    let vaddr_u64 = vaddr as u64;
    let mem_size_u64 = mem_size as u64;

    // Check for overflow in address calculation
    if vaddr_u64.checked_add(mem_size_u64).is_none() {
        return Err(LoadError::InvalidFormat);
    }

    // Ensure mem_size >= file_size
    if mem_size < file_size {
        return Err(LoadError::InvalidFormat);
    }

    let start_addr = VirtAddr::new(vaddr_u64);

    if !is_user_address(start_addr) {
        return Err(LoadError::UnsupportedArchitecture);
    }

    // Allocate user memory for the segment at the specified virtual address
    // Note: For now, we ignore the specified vaddr and let allocate_user_memory choose
    // In a full implementation, we'd allocate at the fixed vaddr
    let allocated_start = allocate_user_memory(page_table, mem_size)?;

    // Copy file data (now that memory is allocated)
    let src = &image_data[file_offset..file_offset + file_size];
    let dest = allocated_start.as_u64() as usize as *mut u8;

    unsafe {
        ptr::copy_nonoverlapping(src.as_ptr(), dest, file_size);
    }

    // Zero out remaining memory if mem_size > file_size
    if mem_size > file_size {
        unsafe {
            ptr::write_bytes(dest.add(file_size), 0, mem_size - file_size);
        }
    }

    Ok(())
}

/// Load error types
#[derive(Debug, Clone, Copy)]
pub enum LoadError {
    InvalidFormat,
    NotExecutable,
    OutOfMemory,
    UnsupportedArchitecture,
    MappingFailed,
}

impl From<crate::memory_management::AllocError> for LoadError {
    fn from(error: crate::memory_management::AllocError) -> Self {
        match error {
            crate::memory_management::AllocError::OutOfMemory => LoadError::OutOfMemory,
            crate::memory_management::AllocError::MappingFailed => LoadError::MappingFailed,
        }
    }
}

/// Initialize the loader
pub fn init() {
    // For now, nothing to initialize
    // Future: Set up any global loader state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalid_format() {
        let invalid_data = [0u8; 64];
        assert!(load_program(&invalid_data, "test").is_err());
    }
}
