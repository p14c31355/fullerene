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
use x86_64::structures::paging::{FrameAllocator, PageTableFlags};

pub const PROGRAM_LOAD_BASE: u64 = 0x400000; // 4MB base address for user programs
const PAGE_SIZE: u64 = 4096;
const LINUX_STACK_SIZE: u64 = 256 * 1024;
const LINUX_STACK_TOP: u64 = 0x0000_7fff_ffff_f000;

#[derive(Clone, Copy)]
struct LinuxImageLayout {
    initial_break: u64,
    phdr: u64,
    phent: u64,
    phnum: u64,
    entry: u64,
}

fn align_down(value: u64) -> u64 {
    value & !(PAGE_SIZE - 1)
}

fn align_up(value: u64) -> Option<u64> {
    value.checked_add(PAGE_SIZE - 1).map(align_down)
}

fn segment_page_flags(flags: u32) -> PageTableFlags {
    let mut result = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if (flags & PF_W) != 0 {
        result |= PageTableFlags::WRITABLE;
    }
    if (flags & PF_X) == 0 {
        result |= PageTableFlags::NO_EXECUTE;
    }
    result
}

fn merge_segment_page_flags(current: PageTableFlags, additional: PageTableFlags) -> PageTableFlags {
    let mut merged = current | PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if additional.contains(PageTableFlags::WRITABLE) {
        merged |= PageTableFlags::WRITABLE;
    }
    if !additional.contains(PageTableFlags::NO_EXECUTE) {
        merged.remove(PageTableFlags::NO_EXECUTE);
    }
    merged
}

fn write_process_bytes(
    page_table: &ProcessPageTable,
    address: u64,
    data: &[u8],
) -> Result<(), LoadError> {
    let mut written = 0usize;
    while written < data.len() {
        let virtual_address = address
            .checked_add(written as u64)
            .ok_or(LoadError::InvalidFormat)?;
        let page_remaining = (PAGE_SIZE - (virtual_address & (PAGE_SIZE - 1))) as usize;
        let chunk_len = page_remaining.min(data.len() - written);
        let physical_address =
            PageTableHelper::translate_address(page_table, virtual_address as usize)
                .map_err(|_| LoadError::MappingFailed)?;
        let kernel_address =
            petroleum::common::memory::physical_to_virtual(physical_address) as *mut u8;
        unsafe {
            ptr::copy_nonoverlapping(data[written..].as_ptr(), kernel_address, chunk_len);
        }
        written += chunk_len;
    }
    Ok(())
}

fn write_stack_word(
    page_table: &ProcessPageTable,
    address: u64,
    value: u64,
) -> Result<(), LoadError> {
    write_process_bytes(page_table, address, &value.to_ne_bytes())
}

fn load_elf_segments(
    page_table: &mut ProcessPageTable,
    elf: &goblin::elf::Elf<'_>,
    image_data: &[u8],
) -> Result<LinuxImageLayout, LoadError> {
    let mut image_end = 0u64;

    for ph in elf.program_headers.iter().filter(|ph| ph.p_type == PT_LOAD) {
        let file_offset = usize::try_from(ph.p_offset).map_err(|_| LoadError::InvalidFormat)?;
        let file_size = usize::try_from(ph.p_filesz).map_err(|_| LoadError::InvalidFormat)?;
        let memory_size = usize::try_from(ph.p_memsz).map_err(|_| LoadError::InvalidFormat)?;
        if memory_size < file_size {
            return Err(LoadError::InvalidFormat);
        }
        let file_end = file_offset
            .checked_add(file_size)
            .ok_or(LoadError::InvalidFormat)?;
        if file_end > image_data.len() {
            return Err(LoadError::InvalidFormat);
        }
        if memory_size == 0 {
            continue;
        }

        let segment_end = ph
            .p_vaddr
            .checked_add(ph.p_memsz)
            .ok_or(LoadError::InvalidFormat)?;
        let page_start = align_down(ph.p_vaddr);
        let page_end = align_up(segment_end).ok_or(LoadError::InvalidFormat)?;
        if page_end <= page_start {
            return Err(LoadError::InvalidFormat);
        }
        let start_address =
            x86_64::VirtAddr::try_new(page_start).map_err(|_| LoadError::InvalidFormat)?;
        let end_address =
            x86_64::VirtAddr::try_new(page_end - 1).map_err(|_| LoadError::InvalidFormat)?;
        if !petroleum::is_user_address(start_address) || !petroleum::is_user_address(end_address) {
            return Err(LoadError::UnsupportedArchitecture);
        }

        let requested_flags = segment_page_flags(ph.p_flags);
        let mut page_address = page_start;
        while page_address < page_end {
            let physical_address =
                match PageTableHelper::translate_address(page_table, page_address as usize) {
                    Ok(existing) => {
                        let current_flags =
                            PageTableHelper::get_page_flags(page_table, page_address as usize)
                                .map_err(|_| LoadError::MappingFailed)?;
                        PageTableHelper::set_page_flags(
                            page_table,
                            page_address as usize,
                            merge_segment_page_flags(current_flags, requested_flags),
                        )
                        .map_err(|_| LoadError::MappingFailed)?;
                        existing
                    }
                    Err(_) => {
                        let frame_allocator =
                            unsafe { petroleum::page_table::constants::get_frame_allocator_mut() };
                        let frame = frame_allocator
                            .allocate_frame()
                            .ok_or(LoadError::OutOfMemory)?;
                        PageTableHelper::map_page(
                            page_table,
                            page_address as usize,
                            frame.start_address().as_u64() as usize,
                            requested_flags,
                            frame_allocator,
                        )
                        .map_err(|_| LoadError::OutOfMemory)?;
                        let physical_address = frame.start_address().as_u64() as usize;
                        unsafe {
                            ptr::write_bytes(
                                petroleum::common::memory::physical_to_virtual(physical_address)
                                    as *mut u8,
                                0,
                                PAGE_SIZE as usize,
                            );
                        }
                        physical_address
                    }
                };

            let file_virtual_end = ph
                .p_vaddr
                .checked_add(ph.p_filesz)
                .ok_or(LoadError::InvalidFormat)?;
            let copy_start = page_address.max(ph.p_vaddr);
            let copy_end = (page_address + PAGE_SIZE).min(file_virtual_end);
            if copy_start < copy_end {
                let source_offset = ph
                    .p_offset
                    .checked_add(copy_start - ph.p_vaddr)
                    .and_then(|offset| usize::try_from(offset).ok())
                    .ok_or(LoadError::InvalidFormat)?;
                let copy_len =
                    usize::try_from(copy_end - copy_start).map_err(|_| LoadError::InvalidFormat)?;
                let source_end = source_offset
                    .checked_add(copy_len)
                    .ok_or(LoadError::InvalidFormat)?;
                if source_end > image_data.len() {
                    return Err(LoadError::InvalidFormat);
                }
                let destination_offset = usize::try_from(copy_start - page_address)
                    .map_err(|_| LoadError::InvalidFormat)?;
                let kernel_page =
                    petroleum::common::memory::physical_to_virtual(physical_address) as *mut u8;
                unsafe {
                    ptr::copy_nonoverlapping(
                        image_data[source_offset..source_end].as_ptr(),
                        kernel_page.add(destination_offset),
                        copy_len,
                    );
                }
            }
            page_address += PAGE_SIZE;
        }

        image_end = image_end.max(segment_end);
    }

    if image_end == 0 {
        return Err(LoadError::InvalidFormat);
    }

    let phdr_size = u64::from(elf.header.e_phentsize)
        .checked_mul(u64::from(elf.header.e_phnum))
        .ok_or(LoadError::InvalidFormat)?;
    let phdr_file_end = elf
        .header
        .e_phoff
        .checked_add(phdr_size)
        .ok_or(LoadError::InvalidFormat)?;
    let phdr = elf
        .program_headers
        .iter()
        .filter(|ph| ph.p_type == PT_LOAD)
        .find_map(|ph| {
            let load_file_end = ph.p_offset.checked_add(ph.p_filesz)?;
            (elf.header.e_phoff >= ph.p_offset && phdr_file_end <= load_file_end)
                .then(|| ph.p_vaddr + (elf.header.e_phoff - ph.p_offset))
        })
        .ok_or(LoadError::InvalidFormat)?;

    Ok(LinuxImageLayout {
        initial_break: align_up(image_end).ok_or(LoadError::InvalidFormat)?,
        phdr,
        phent: u64::from(elf.header.e_phentsize),
        phnum: u64::from(elf.header.e_phnum),
        entry: elf.entry,
    })
}

fn initialize_linux_stack(
    page_table: &mut ProcessPageTable,
    program_name: &str,
    layout: LinuxImageLayout,
) -> Result<u64, LoadError> {
    let stack_bottom = LINUX_STACK_TOP
        .checked_sub(LINUX_STACK_SIZE)
        .ok_or(LoadError::InvalidFormat)?;
    let stack_flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE;

    let mut page_address = stack_bottom;
    while page_address < LINUX_STACK_TOP {
        if PageTableHelper::translate_address(page_table, page_address as usize).is_ok() {
            return Err(LoadError::AddressAlreadyMapped);
        }
        let frame_allocator =
            unsafe { petroleum::page_table::constants::get_frame_allocator_mut() };
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(LoadError::OutOfMemory)?;
        PageTableHelper::map_page(
            page_table,
            page_address as usize,
            frame.start_address().as_u64() as usize,
            stack_flags,
            frame_allocator,
        )
        .map_err(|_| LoadError::OutOfMemory)?;
        unsafe {
            ptr::write_bytes(
                petroleum::common::memory::physical_to_virtual(
                    frame.start_address().as_u64() as usize
                ) as *mut u8,
                0,
                PAGE_SIZE as usize,
            );
        }
        page_address += PAGE_SIZE;
    }

    let mut cursor = LINUX_STACK_TOP;
    let name_bytes = program_name.as_bytes();
    cursor = cursor
        .checked_sub(name_bytes.len() as u64 + 1)
        .ok_or(LoadError::InvalidFormat)?;
    let argv0 = cursor;
    write_process_bytes(page_table, argv0, name_bytes)?;
    write_process_bytes(page_table, argv0 + name_bytes.len() as u64, &[0])?;

    cursor = cursor.checked_sub(16).ok_or(LoadError::InvalidFormat)?;
    let random_address = cursor;
    let seed = unsafe { core::arch::x86_64::_rdtsc() };
    let random = [
        seed.to_ne_bytes(),
        seed.rotate_left(29)
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .to_ne_bytes(),
    ]
    .concat();
    write_process_bytes(page_table, random_address, &random)?;

    cursor &= !15;
    const AT_NULL: u64 = 0;
    const AT_PHDR: u64 = 3;
    const AT_PHENT: u64 = 4;
    const AT_PHNUM: u64 = 5;
    const AT_PAGESZ: u64 = 6;
    const AT_BASE: u64 = 7;
    const AT_FLAGS: u64 = 8;
    const AT_ENTRY: u64 = 9;
    const AT_UID: u64 = 11;
    const AT_EUID: u64 = 12;
    const AT_GID: u64 = 13;
    const AT_EGID: u64 = 14;
    const AT_CLKTCK: u64 = 17;
    const AT_SECURE: u64 = 23;
    const AT_RANDOM: u64 = 25;
    const AT_EXECFN: u64 = 31;

    let words = [
        1,
        argv0,
        0,
        0,
        AT_PHDR,
        layout.phdr,
        AT_PHENT,
        layout.phent,
        AT_PHNUM,
        layout.phnum,
        AT_PAGESZ,
        PAGE_SIZE,
        AT_BASE,
        0,
        AT_FLAGS,
        0,
        AT_ENTRY,
        layout.entry,
        AT_UID,
        0,
        AT_EUID,
        0,
        AT_GID,
        0,
        AT_EGID,
        0,
        AT_CLKTCK,
        100,
        AT_SECURE,
        0,
        AT_RANDOM,
        random_address,
        AT_EXECFN,
        argv0,
        AT_NULL,
        0,
    ];
    let words_size = (words.len() * core::mem::size_of::<u64>()) as u64;
    let rsp = cursor
        .checked_sub(words_size)
        .ok_or(LoadError::InvalidFormat)?
        & !15;
    for (index, value) in words.into_iter().enumerate() {
        write_stack_word(page_table, rsp + (index as u64 * 8), value)?;
    }
    Ok(rsp)
}

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

    // The first Linux personality milestone intentionally supports static
    // x86_64 executables.  A PT_INTERP image would require a dynamic linker,
    // relocation, and a second ELF load transaction.
    if elf.header.e_type != goblin::elf::header::ET_EXEC {
        return Err(LoadError::NotExecutable);
    }
    if elf.header.e_machine != goblin::elf::header::EM_X86_64 {
        return Err(LoadError::UnsupportedArchitecture);
    }
    if is_linux && elf.interpreter.is_some() {
        return Err(LoadError::NotExecutable);
    }

    let entry_point_address =
        x86_64::VirtAddr::try_new(elf.header.e_entry).map_err(|_| LoadError::InvalidFormat)?;
    if !petroleum::is_user_address(entry_point_address) {
        return Err(LoadError::UnsupportedArchitecture);
    }

    // Create process with the loaded program (user mode)
    let pid = process::create_process(name, entry_point_address, true)?;

    process::SCHEDULER
        .with_process(pid, |p| {
            let layout = {
                let process_page_table = p.page_table.as_mut().ok_or(LoadError::InvalidFormat)?;
                load_elf_segments(process_page_table, &elf, image_data)?
            };

            if is_linux {
                let rsp = {
                    let process_page_table =
                        p.page_table.as_mut().ok_or(LoadError::InvalidFormat)?;
                    initialize_linux_stack(process_page_table, name, layout)?
                };

                // `create_process` supplies the legacy native-user stack from
                // the kernel heap.  Linux processes use the mapped lower-half
                // stack above, so release the unused allocation.
                if let Some(old_stack_base) = p
                    .user_stack
                    .as_u64()
                    .checked_sub(crate::heap::KERNEL_STACK_SIZE as u64)
                    .filter(|&base| base != 0)
                {
                    let stack_layout =
                        core::alloc::Layout::from_size_align(crate::heap::KERNEL_STACK_SIZE, 16)
                            .map_err(|_| LoadError::InvalidFormat)?;
                    unsafe {
                        petroleum::common::memory::deallocate_layout(
                            old_stack_base as *mut u8,
                            stack_layout,
                        );
                    }
                }

                p.user_stack = x86_64::VirtAddr::new(LINUX_STACK_TOP);
                p.context.regs[7] = rsp;
                // Linux personality processes are currently cooperative. The
                // kernel's Rust `x86-interrupt` handlers cannot yet return
                // reliably to CPL3, so do not expose hardware interrupts
                // until that generic interrupt-return path is repaired.
                // SYSCALL/SYSRET preserves this choice in R11.
                p.context.rflags = 0x2;
                let runtime = crate::linux::LinuxRuntime::new(p.id.0, layout.initial_break);
                p.dispatch_mode = Some(crate::linux::DispatchMode::Linux(alloc::boxed::Box::new(
                    runtime,
                )));
            }
            Ok::<(), LoadError>(())
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
    FileNotFound,
}

impl From<LoadError> for petroleum::common::logging::SystemError {
    fn from(error: LoadError) -> Self {
        match error {
            LoadError::InvalidFormat => petroleum::common::logging::SystemError::InvalidFormat,
            LoadError::OutOfMemory => petroleum::common::logging::SystemError::MemOutOfMemory,
            LoadError::AddressAlreadyMapped => {
                petroleum::common::logging::SystemError::MappingFailed
            }
            LoadError::FileNotFound => petroleum::common::logging::SystemError::FileNotFound,
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
