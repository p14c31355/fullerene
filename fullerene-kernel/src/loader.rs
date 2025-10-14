//! Program loader for Fullerene OS
//!
//! This module is responsible for loading executable programs into memory
//! and creating processes to run them.

use crate::memory_management::ProcessPageTable;
use crate::process;
use crate::errors::SystemError;
use crate::traits::PageTableHelper;
use core::ptr;
use x86_64::structures::paging::FrameAllocator;
use x86_64::structures::paging::PageTableFlags as PageFlags;

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
const PF_R: u32 = 0x4;
const PF_W: u32 = 0x2;
const PF_X: u32 = 0x1;

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
    let entry_point_address = x86_64::VirtAddr::new(elf_header.entry_point);

    // Create process with the loaded program
    let pid = process::create_process(name, entry_point_address);

    // Get the process's page table (assume it's created in create_process)
    // For now, we skip loading segments due to page table integration not implemented yet
    // let mut process_list_locked = process::PROCESS_LIST.lock();
    // let process = process_list_locked
    //     .iter_mut()
    //     .find(|p| p.id == pid)
    //     .unwrap();
    // let process_page_table = &mut process.page_table.as_mut().unwrap();

    // Get the process's page table
    let process_list_locked = process::PROCESS_LIST.lock();
    let process = process_list_locked
        .iter()
        .find(|p| p.id == pid)
        .ok_or(LoadError::InvalidFormat)?;
    let process_page_table = &process.page_table;

    // Load program segments
    for i in 0..ph_count {
        let ph_offset = ph_offset + i * ph_entry_size;
        if ph_offset + core::mem::size_of::<ProgramHeader>() > image_data.len() {
            return Err(LoadError::InvalidFormat);
        }

        let ph = unsafe { &*(image_data.as_ptr().add(ph_offset) as *const ProgramHeader) };

        // Only load PT_LOAD segments
        if ph.p_type == PT_LOAD {
            load_segment(ph, image_data, process_page_table.as_ref().ok_or(LoadError::InvalidFormat)?)?;
        }
    }

    Ok(pid)
}

/// Load a program segment into memory
fn load_segment(
    ph: &ProgramHeader,
    image_data: &[u8],
    page_table: &ProcessPageTable,
) -> Result<(), LoadError> {
    let file_offset = ph.offset as usize;
    let file_size = ph.file_size as usize;
    let mem_size = ph.mem_size as usize;
    let vaddr = ph.vaddr as u64;

    // Bounds check
    if file_offset + file_size > image_data.len() {
        return Err(LoadError::InvalidFormat);
    }

    // Ensure mem_size >= file_size and check for overflow
    if mem_size < file_size || vaddr.checked_add(mem_size as u64).is_none() {
        return Err(LoadError::InvalidFormat);
    }

    // Validate that the virtual address range is in user space
    use crate::memory_management::{is_user_address, map_user_page};
    use x86_64::VirtAddr;

    let start_addr = VirtAddr::new(vaddr);
    let end_addr = VirtAddr::new(vaddr + mem_size as u64 - 1);

    if !is_user_address(start_addr) || !is_user_address(end_addr) {
        return Err(LoadError::UnsupportedArchitecture);
    }

    let num_pages = ((mem_size + 4095) / 4096) as u64; // Round up to page size

    // Check that the virtual address range is not already mapped
    for page_idx in 0..num_pages {
        let page_vaddr = VirtAddr::new(vaddr + page_idx * 4096);
        if page_table.translate_address(page_vaddr.as_u64() as usize).is_ok() {
            return Err(LoadError::AddressAlreadyMapped);
        }
    }

    // For each page needed by the segment, allocate a physical frame and map it
    for page_idx in 0..num_pages {
        let page_vaddr = VirtAddr::new(vaddr + page_idx * 4096);

        // Allocate a physical frame for this page
        let frame = crate::heap::paging::FRAME_ALLOCATOR
            .get()
            .unwrap()
            .lock()
            .allocate_frame()
            .ok_or(LoadError::OutOfMemory)?;

        // Map the virtual page to the physical frame
        use x86_64::structures::paging::PageTableFlags as X86Flags;
        let mut page_flags = X86Flags::PRESENT | X86Flags::USER_ACCESSIBLE;
        if ph.flags & PF_W != 0 {
            page_flags |= X86Flags::WRITABLE;
        }
        if ph.flags & PF_X == 0 {
            page_flags |= X86Flags::NO_EXECUTE;
        }

        map_user_page(
            page_vaddr.as_u64() as usize,
            frame.start_address().as_u64() as usize,
            page_flags,
        )?;
    }

    // Now copy the file data to the allocated virtual memory.
    // We use a guard to safely switch to the process's page table and back.
    struct Cr3SwitchGuard {
        original_cr3: x86_64::structures::paging::PhysFrame,
        original_cr3_flags: x86_64::registers::control::Cr3Flags,
    }

    impl Cr3SwitchGuard {
        unsafe fn new(page_table: &ProcessPageTable) -> Self {
            let (original_cr3, original_cr3_flags) = x86_64::registers::control::Cr3::read();
            crate::memory_management::switch_to_page_table(page_table);
            Self {
                original_cr3,
                original_cr3_flags,
            }
        }
    }

    impl Drop for Cr3SwitchGuard {
        fn drop(&mut self) {
            unsafe {
                x86_64::registers::control::Cr3::write(self.original_cr3, self.original_cr3_flags);
            }
        }
    }

    let _cr3_guard = unsafe { Cr3SwitchGuard::new(page_table) };

    // Copy file data
    let src = &image_data[file_offset..file_offset + file_size];
    let dest = vaddr as *mut u8;

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
    AddressAlreadyMapped,
}

impl From<crate::memory_management::AllocError> for LoadError {
    fn from(error: crate::memory_management::AllocError) -> Self {
        match error {
            crate::memory_management::AllocError::OutOfMemory => LoadError::OutOfMemory,
            crate::memory_management::AllocError::MappingFailed => LoadError::MappingFailed,
        }
    }
}

impl From<crate::memory_management::MapError> for LoadError {
    fn from(error: crate::memory_management::MapError) -> Self {
        match error {
            crate::memory_management::MapError::MappingFailed => LoadError::MappingFailed,
            crate::memory_management::MapError::UnmappingFailed => LoadError::MappingFailed,
            crate::memory_management::MapError::FrameAllocationFailed => LoadError::OutOfMemory,
        }
    }
}

impl From<crate::memory_management::FreeError> for LoadError {
    fn from(error: crate::memory_management::FreeError) -> Self {
        match error {
            crate::memory_management::FreeError::UnmappingFailed => LoadError::MappingFailed,
        }
    }
}

impl From<SystemError> for LoadError {
    fn from(error: SystemError) -> Self {
        match error {
            SystemError::MemOutOfMemory => LoadError::OutOfMemory,
            SystemError::InvalidArgument => LoadError::InvalidFormat,
            SystemError::InternalError => LoadError::MappingFailed,
            _ => LoadError::MappingFailed,
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
