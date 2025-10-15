//! Program loader for Fullerene OS
//!
//! This module is responsible for loading executable programs into memory
//! and creating processes to run them.

use crate::memory_management::ProcessPageTable;
use crate::process;
use crate::traits::PageTableHelper;
use core::ptr;
use x86_64::structures::paging::FrameAllocator;
use x86_64::structures::paging::PageTableFlags as PageFlags;
use goblin::elf::program_header::{PF_W, PF_X, PT_LOAD};

pub const PROGRAM_LOAD_BASE: u64 = 0x400000; // 4MB base address for user programs

/// Load a program from raw bytes and create a process for it using goblin
pub fn load_program(
    image_data: &[u8],
    name: &'static str,
) -> Result<process::ProcessId, LoadError> {
    // Parse ELF using goblin
    let elf = goblin::elf::Elf::parse(image_data).map_err(|_| LoadError::InvalidFormat)?;

    // Verify this is an executable
    if elf.header.e_type != goblin::elf::header::ET_EXEC {
        return Err(LoadError::NotExecutable);
    }

    // Find entry point
    let entry_point_address = x86_64::VirtAddr::new(elf.header.e_entry);

    // Create process with the loaded program
    let pid = process::create_process(name, entry_point_address);

    // Get the process's page table
    let process_list_locked = process::PROCESS_LIST.lock();
    let process = process_list_locked
        .iter()
        .find(|p| p.id == pid)
        .ok_or(LoadError::InvalidFormat)?;
    let process_page_table = &process.page_table;

    // Load program segments using goblin
    for ph in &elf.program_headers {
        // Only load PT_LOAD segments
        if ph.p_type == PT_LOAD {
            // Load the segment data inline
            let file_offset = ph.p_offset as usize;
            let file_size = ph.p_filesz as usize;
            let mem_size = ph.p_memsz as usize;
            let vaddr = ph.p_vaddr as u64;

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

            let start_addr = x86_64::VirtAddr::new(vaddr);
            let end_addr = x86_64::VirtAddr::new(vaddr + mem_size as u64 - 1);

            if !is_user_address(start_addr) || !is_user_address(end_addr) {
                return Err(LoadError::UnsupportedArchitecture);
            }

            let num_pages = ((mem_size + 4095) / 4096) as u64; // Round up to page size

            // Check that the virtual address range is not already mapped
            if let Some(pt) = process_page_table {
                for page_idx in 0..num_pages {
                    let page_vaddr = x86_64::VirtAddr::new(vaddr + page_idx * 4096);
                    if pt.translate_address(page_vaddr.as_u64() as usize).is_ok() {
                        return Err(LoadError::AddressAlreadyMapped);
                    }
                }
            }

            // For each page needed by the segment, allocate a physical frame and map it
            for page_idx in 0..num_pages {
                let page_vaddr = x86_64::VirtAddr::new(vaddr + page_idx * 4096);

                // Allocate a physical frame for this page
            use x86_64::structures::paging::PhysFrame;
            let frame = match crate::heap::MEMORY_MAP.get() {
                Some(memory_map) => {
                    crate::heap::init_frame_allocator(*memory_map);
                    if let Some(allocator_ref) = crate::heap::memory_map::FRAME_ALLOCATOR.get() {
                        allocator_ref.lock().allocate_frame().ok_or(LoadError::OutOfMemory)?
                    } else {
                        return Err(LoadError::OutOfMemory);
                    }
                }
                None => return Err(LoadError::OutOfMemory),
            };

                // Map the virtual page to the physical frame
                use x86_64::structures::paging::PageTableFlags as X86Flags;
                let mut page_flags = X86Flags::PRESENT | X86Flags::USER_ACCESSIBLE;
                if (ph.p_flags & PF_W) != 0 {
                    page_flags |= X86Flags::WRITABLE;
                }
                if (ph.p_flags & PF_X) == 0 {
                    page_flags |= X86Flags::NO_EXECUTE;
                }
                if (ph.p_flags & PF_X) != 0 {
                    // Clear NO_EXECUTE if executable
                    page_flags.remove(X86Flags::NO_EXECUTE);
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

            let process_page_table = process_page_table.as_ref().ok_or(LoadError::InvalidFormat)?;
            let _cr3_guard = unsafe { Cr3SwitchGuard::new(process_page_table) };

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
        }
    }

    Ok(pid)
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



impl From<petroleum::common::logging::SystemError> for LoadError {
    fn from(error: petroleum::common::logging::SystemError) -> Self {
        match error {
            // Map petrochemical SystemError to kernel SystemError first
            petroleum::common::logging::SystemError::MemOutOfMemory => LoadError::OutOfMemory,
            petroleum::common::logging::SystemError::InvalidArgument => LoadError::InvalidFormat,
            petroleum::common::logging::SystemError::InternalError => LoadError::MappingFailed,
            petroleum::common::logging::SystemError::MappingFailed => LoadError::MappingFailed,
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
