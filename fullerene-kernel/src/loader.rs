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

    // Load program segments
    for i in 0..ph_count {
        let ph_offset = ph_offset + i * ph_entry_size;
        if ph_offset + core::mem::size_of::<ProgramHeader>() > image_data.len() {
            return Err(LoadError::InvalidFormat);
        }

        let ph = unsafe { &*(image_data.as_ptr().add(ph_offset) as *const ProgramHeader) };

        // Only load PT_LOAD segments
        if ph.p_type == PT_LOAD {
            load_segment(ph, image_data)?;
        }
    }

    // Find entry point
    let entry_point: fn() = unsafe { core::mem::transmute(elf_header.entry_point as usize) };

    // Create process with the loaded program
    let pid = process::create_process(name, entry_point);

    Ok(pid)
}

/// Load a program segment into memory
fn load_segment(ph: &ProgramHeader, image_data: &[u8]) -> Result<(), LoadError> {
    let file_offset = ph.offset as usize;
    let file_size = ph.file_size as usize;
    let mem_size = ph.mem_size as usize;
    let vaddr = ph.vaddr as usize;

    // Bounds check
    if file_offset + file_size > image_data.len() {
        return Err(LoadError::InvalidFormat);
    }

    // Allocate memory for segment (for now, just copy to a fixed virtual address)
    // In a real system, we'd allocate proper virtual memory pages

    // Copy file data
    let src = &image_data[file_offset..file_offset + file_size];
    let dest = (PROGRAM_LOAD_BASE as usize + vaddr) as *mut u8;

    // For now, we'll simulate memory allocation by just checking if the destination
    // is accessible. In practice, this needs proper virtual memory management.

    // TODO: Replace with proper memory allocation once virtual memory is implemented
    unsafe {
        // Check if we think this memory is available (very simplistic check)
        // In real kernel, this would involve page table checks
        if dest as usize >= 0x100000 {
            // Above 1MB should be safer
            ptr::copy_nonoverlapping(src.as_ptr(), dest, file_size);
        } else {
            return Err(LoadError::OutOfMemory);
        }
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
