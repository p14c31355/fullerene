//! Program loader for Fullerene OS
//!
//! This module is responsible for loading executable programs into memory
//! and creating processes to run them.
//!
//! # Memory separation
//!
//! The loader writes segment data directly into the physical frames
//! backing the process page table, using `physical_to_virtual` to map
//! the kernel's direct-mapped view of the frames.  This avoids the need
//! to switch CR3 to the process page table during loading, which is
//! unsafe and racy in a preemptible kernel.

use crate::process;
use core::ptr;
use goblin::elf::program_header::{PF_W, PF_X, PT_LOAD};
use petroleum::page_table::process::ProcessPageTable;
use petroleum::page_table::types::PageTableHelper;
use x86_64::structures::paging::FrameAllocator;

pub const PROGRAM_LOAD_BASE: u64 = 0x400000; // 4MB base address for user programs

/// Load a program from raw bytes and create a process for it using goblin.
/// If `linux_abi` is true, attaches a LinuxRuntime for Linux ABI emulation.
pub fn load_program(
    image_data: &[u8],
    name: &'static str,
) -> Result<process::ProcessId, LoadError> {
    load_program_inner(image_data, name, false)
}

/// Load a program, optionally with Linux ABI emulation.
pub fn load_program_with_runtime(
    image_data: &[u8],
    name: &'static str,
    is_linux: bool,
) -> Result<process::ProcessId, LoadError> {
    load_program_inner(image_data, name, is_linux)
}

fn load_program_inner(
    image_data: &[u8],
    name: &'static str,
    is_linux: bool,
) -> Result<process::ProcessId, LoadError> {
    // Parse ELF using goblin
    let elf = goblin::elf::Elf::parse(image_data).map_err(|_| LoadError::InvalidFormat)?;

    // Verify this is an executable
    if elf.header.e_type != goblin::elf::header::ET_EXEC {
        return Err(LoadError::NotExecutable);
    }

    // Find entry point
    let entry_point_address = x86_64::VirtAddr::new(elf.header.e_entry);

    // Create process with the loaded program (user mode)
    let pid = process::create_process(name, entry_point_address, true)?;

    // Attach LinuxRuntime if this is a Linux binary
    if is_linux {
        process::PROCESS_MANAGER.with_process(pid, |p| {
            let initial_break = 0x60000000u64;
            let rt = crate::linux::LinuxRuntime::new(p.id.0, initial_break);
            p.dispatch_mode = Some(crate::linux::DispatchMode::Linux(rt));
        });
    }

    // Load program segments into the process page table.
    //
    // We write each PT_LOAD segment into the freshly allocated physical
    // frame using `physical_to_virtual`, which gives us a kernel-visible
    // pointer to the user-space page.  This avoids switching CR3 to the
    // process page table during load and keeps the kernel's address space
    // active.
    process::PROCESS_MANAGER
        .with_process(pid, |p| {
            let process_page_table = p.page_table.as_mut().ok_or(LoadError::InvalidFormat)?;

            for ph in &elf.program_headers {
                if ph.p_type != PT_LOAD {
                    continue;
                }
                let file_offset = ph.p_offset as usize;
                let file_size = ph.p_filesz as usize;
                let mem_size = ph.p_memsz as usize;
                let vaddr = ph.p_vaddr as u64;

                // Check file range with overflow protection
                let file_end = file_offset.checked_add(file_size)
                    .ok_or(LoadError::InvalidFormat)?;
                if file_end > image_data.len() {
                    return Err(LoadError::InvalidFormat);
                }
                if mem_size < file_size {
                    return Err(LoadError::InvalidFormat);
                }
                // Check virtual address range with overflow protection
                let vaddr_end = vaddr.checked_add(mem_size as u64)
                    .ok_or(LoadError::InvalidFormat)?;
                if mem_size == 0 {
                    return Err(LoadError::InvalidFormat);
                }
                let start_addr = x86_64::VirtAddr::new(vaddr);
                let end_addr = x86_64::VirtAddr::new(vaddr_end - 1);
                if !petroleum::is_user_address(start_addr) || !petroleum::is_user_address(end_addr)
                {
                    return Err(LoadError::UnsupportedArchitecture);
                }
                let num_pages = petroleum::common::utils::calculate_pages(mem_size);

                // Check that the virtual address range is not already mapped.
                for page_idx in 0..num_pages {
                    let page_vaddr = x86_64::VirtAddr::new(
                        petroleum::common::utils::calculate_offset_address(vaddr, page_idx),
                    );
                    let ppt: &ProcessPageTable = &**process_page_table;
                    if PageTableHelper::translate_address(ppt, page_vaddr.as_u64() as usize).is_ok()
                    {
                        return Err(LoadError::AddressAlreadyMapped);
                    }
                }

                // For each page needed by the segment, allocate a physical frame,
                // map it into the process page table, then write the segment data
                // via the kernel's direct-mapped view of the frame.
                use x86_64::structures::paging::PageTableFlags as X86Flags;
                for page_idx in 0..num_pages {
                    let page_vaddr = x86_64::VirtAddr::new(
                        petroleum::common::utils::calculate_offset_address(vaddr, page_idx),
                    );
                    let frame = crate::heap::FRAME_ALLOCATOR
                        .lock()
                        .as_mut()
                        .ok_or(LoadError::OutOfMemory)?
                        .allocate_frame()
                        .ok_or(LoadError::OutOfMemory)?;
                    let mut page_flags = X86Flags::PRESENT | X86Flags::USER_ACCESSIBLE;
                    if (ph.p_flags & PF_W) != 0 {
                        page_flags |= X86Flags::WRITABLE;
                    }
                    if (ph.p_flags & PF_X) == 0 {
                        page_flags |= X86Flags::NO_EXECUTE;
                    }
                    PageTableHelper::map_page(
                        &mut **process_page_table,
                        page_vaddr.as_u64() as usize,
                        frame.start_address().as_u64() as usize,
                        page_flags,
                        petroleum::page_table::constants::get_frame_allocator_mut(),
                    )
                    .map_err(|_| LoadError::OutOfMemory)?;

                    // Write directly through the kernel's direct-mapped view of
                    // the physical frame.  This does NOT require a CR3 switch —
                    // the kernel's page table always maps all physical memory at
                    // `physical_memory_offset`.
                    let frame_phys = frame.start_address().as_u64() as usize;
                    let frame_vaddr = petroleum::common::memory::physical_to_virtual(frame_phys);
                    let page_offset = (page_idx * 4096) as u64;
                    unsafe {
                        if page_offset < file_size as u64 {
                            let copy_len = ((file_size as u64) - page_offset).min(4096) as usize;
                            let src_offset = (file_offset as u64 + page_offset) as usize;
                            ptr::copy_nonoverlapping(
                                image_data[src_offset..src_offset + copy_len].as_ptr(),
                                frame_vaddr as *mut u8,
                                copy_len,
                            );
                            if copy_len < 4096 {
                                ptr::write_bytes(
                                    (frame_vaddr as *mut u8).add(copy_len),
                                    0,
                                    4096 - copy_len,
                                );
                            }
                        } else {
                            // Zero-fill BSS page entirely.
                            ptr::write_bytes(frame_vaddr as *mut u8, 0, 4096);
                        }
                    }
                }
            }
            Ok(())
        })
        .ok_or(LoadError::InvalidFormat)??;

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

impl From<LoadError> for petroleum::common::logging::SystemError {
    fn from(error: LoadError) -> Self {
        match error {
            LoadError::InvalidFormat => petroleum::common::logging::SystemError::InvalidFormat,
            LoadError::OutOfMemory => petroleum::common::logging::SystemError::MemOutOfMemory,
            LoadError::AddressAlreadyMapped => {
                petroleum::common::logging::SystemError::MappingFailed
            }
            LoadError::MappingFailed => petroleum::common::logging::SystemError::MappingFailed,
            LoadError::NotExecutable | LoadError::UnsupportedArchitecture => {
                petroleum::common::logging::SystemError::LoadFailed
            }
        }
    }
}

petroleum::error_chain!(crate::memory_management::AllocError, LoadError,
    crate::memory_management::AllocError::OutOfMemory => LoadError::OutOfMemory,
    crate::memory_management::AllocError::MappingFailed => LoadError::MappingFailed,
);

petroleum::error_chain!(crate::memory_management::MapError, LoadError,
    crate::memory_management::MapError::MappingFailed => LoadError::MappingFailed,
    crate::memory_management::MapError::UnmappingFailed => LoadError::MappingFailed,
    crate::memory_management::MapError::FrameAllocationFailed => LoadError::OutOfMemory,
);

petroleum::error_chain!(crate::memory_management::FreeError, LoadError,
    crate::memory_management::FreeError::UnmappingFailed => LoadError::MappingFailed,
);

impl From<petroleum::common::logging::SystemError> for LoadError {
    fn from(error: petroleum::common::logging::SystemError) -> Self {
        match error {
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
