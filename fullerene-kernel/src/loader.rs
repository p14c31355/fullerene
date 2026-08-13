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
use alloc::vec::Vec;
use core::ptr;
use core::sync::atomic::Ordering;
use goblin::elf::program_header::{PF_W, PF_X, PT_LOAD};
use petroleum::page_table::FrameAllocatorExt;
use petroleum::page_table::process::ProcessPageTable;
use petroleum::page_table::types::PageTableHelper;
use x86_64::structures::paging::{FrameAllocator, PageTableFlags};

pub const PROGRAM_LOAD_BASE: u64 = 0x400000; // 4MB base address for user programs
const PAGE_SIZE: u64 = 4096;
pub const LINUX_STACK_SIZE: u64 = 256 * 1024;
pub const LINUX_STACK_TOP: u64 = 0x0000_7fff_ffff_f000;
// The Linux syscall layer uses the same 64 MiB logical cap. Pages are mapped
// by sys_brk as the logical break grows instead of being committed at exec.
const LINUX_BRK_RESERVE_SIZE: u64 = 64 * 1024 * 1024;
pub const DYNAMIC_LINKER_BASE: u64 = 0x0000_0002_0000_0000;
pub const DYNAMIC_LINKER_RESERVE_SIZE: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
struct LinuxImageLayout {
    initial_break: u64,
    phdr: u64,
    phent: u64,
    phnum: u64,
    entry: u64,
    at_base: u64,
}

struct LoadedLinuxImage {
    layout: LinuxImageLayout,
    changes: Vec<PageChange>,
}

#[derive(Clone, Copy)]
enum PageChange {
    New {
        address: u64,
    },
    Replaced {
        address: u64,
        old_frame: u64,
        old_flags: PageTableFlags,
    },
}

impl PageChange {
    fn address(self) -> u64 {
        match self {
            Self::New { address } => address,
            Self::Replaced { address, .. } => address,
        }
    }
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
    if merged.contains(PageTableFlags::WRITABLE) && !merged.contains(PageTableFlags::NO_EXECUTE) {
        // A shared PT_LOAD boundary page must never become W+X. Executable
        // access wins over writable access for the ambiguous shared page.
        merged.remove(PageTableFlags::WRITABLE);
    }
    merged
}

fn rollback_page_changes(page_table: &mut ProcessPageTable, changes: &[PageChange]) {
    petroleum::page_table::constants::with_frame_allocator(|frame_allocator| {
        for change in changes.iter().rev() {
            match *change {
                PageChange::New { address } => {
                    if let Ok(frame) = PageTableHelper::unmap_page(page_table, address as usize) {
                        if let Some(frame) = petroleum::page_table::PhysFrame::from_start_address(
                            frame.start_address().as_u64(),
                        ) {
                            frame_allocator.deallocate_frame(frame);
                        }
                    }
                }
                PageChange::Replaced {
                    address,
                    old_frame,
                    old_flags,
                } => {
                    if let Ok(frame) = PageTableHelper::unmap_page(page_table, address as usize) {
                        if let Some(frame) = petroleum::page_table::PhysFrame::from_start_address(
                            frame.start_address().as_u64(),
                        ) {
                            frame_allocator.deallocate_frame(frame);
                        }
                    }
                    let _ = PageTableHelper::map_page(
                        page_table,
                        address as usize,
                        old_frame as usize,
                        old_flags,
                        frame_allocator,
                    );
                }
            }
        }
    });
}

fn release_replaced_frames(changes: &[PageChange]) {
    petroleum::page_table::constants::with_frame_allocator(|frame_allocator| {
        for change in changes {
            if let PageChange::Replaced { old_frame, .. } = *change
                && let Some(frame) = petroleum::page_table::PhysFrame::from_start_address(old_frame)
            {
                frame_allocator.deallocate_frame(frame);
            }
        }
    });
}

fn map_zeroed_page(
    page_table: &mut ProcessPageTable,
    address: u64,
    flags: PageTableFlags,
) -> Result<usize, LoadError> {
    petroleum::page_table::constants::with_frame_allocator(|frame_allocator| {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(LoadError::OutOfMemory)?;
        let physical_address = frame.start_address().as_u64() as usize;
        unsafe {
            ptr::write_bytes(
                petroleum::common::memory::physical_to_virtual(physical_address) as *mut u8,
                0,
                PAGE_SIZE as usize,
            );
        }
        if PageTableHelper::map_page(
            page_table,
            address as usize,
            physical_address,
            flags,
            frame_allocator,
        )
        .is_err()
        {
            let frame = petroleum::page_table::PhysFrame::from_start_address(
                frame.start_address().as_u64(),
            )
            .expect("x86_64 frame addresses are page-aligned");
            frame_allocator.deallocate_frame(frame);
            return Err(LoadError::OutOfMemory);
        }
        Ok(physical_address)
    })
}

/// Ensure that a user-writable mapping has writable ancestors as well as a
/// writable leaf. x86 rejects a user write when any PML4/PDP/PD/PT entry in
/// the walk lacks WRITABLE, even if the final PTE has the bit set.
fn ensure_user_writable_path(
    page_table: &mut ProcessPageTable,
    address: u64,
) -> Result<(), LoadError> {
    let required =
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
    let root = page_table
        .pml4_frame()
        .ok_or(LoadError::MappingFailed)?
        .start_address();
    unsafe {
        crate::memory_management::walk_page_table_entries(root, address, |_, entry| {
            entry.set_flags(entry.flags() | required);
        })
    }
    .map_err(|_| LoadError::MappingFailed)?;

    unsafe {
        core::arch::asm!(
            "invlpg [{}]",
            in(reg) address,
            options(nostack, preserves_flags)
        );
    }
    Ok(())
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

fn load_elf_segments_transaction(
    page_table: &mut ProcessPageTable,
    elf: &goblin::elf::Elf<'_>,
    image_data: &[u8],
    load_bias: u64,
    changes: &mut Vec<PageChange>,
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

        let segment_start = load_bias
            .checked_add(ph.p_vaddr)
            .ok_or(LoadError::InvalidFormat)?;
        let segment_end = segment_start
            .checked_add(ph.p_memsz)
            .ok_or(LoadError::InvalidFormat)?;
        let page_start = align_down(segment_start);
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
            let already_loaded = changes
                .iter()
                .any(|change| change.address() == page_address);
            let physical_address =
                match PageTableHelper::translate_address(page_table, page_address as usize) {
                    Ok(existing) if already_loaded => {
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
                    Ok(_) => {
                        // An execve image must not inherit writable loader or
                        // RELRO state from the previous image. Retain the old
                        // frame in the transaction so rollback can restore it;
                        // a successful exec releases it after ownership has
                        // been established by the Linux fork path.
                        let old_flags =
                            PageTableHelper::get_page_flags(page_table, page_address as usize)
                                .map_err(|_| LoadError::MappingFailed)?;
                        let old_frame =
                            PageTableHelper::unmap_page(page_table, page_address as usize)
                                .map_err(|_| LoadError::MappingFailed)?;
                        changes.push(PageChange::Replaced {
                            address: page_address,
                            old_frame: old_frame.start_address().as_u64(),
                            old_flags,
                        });
                        let physical_address =
                            map_zeroed_page(page_table, page_address, requested_flags)?;
                        physical_address
                    }
                    Err(_) => {
                        let physical_address =
                            map_zeroed_page(page_table, page_address, requested_flags)?;
                        changes.push(PageChange::New {
                            address: page_address,
                        });
                        physical_address
                    }
                };

            let file_virtual_end = segment_start
                .checked_add(ph.p_filesz)
                .ok_or(LoadError::InvalidFormat)?;
            let copy_start = page_address.max(segment_start);
            let copy_end = (page_address + PAGE_SIZE).min(file_virtual_end);
            if copy_start < copy_end {
                let source_offset = ph
                    .p_offset
                    .checked_add(copy_start - segment_start)
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
            let zero_start = page_address.max(copy_end);
            let zero_end = (page_address + PAGE_SIZE).min(segment_end);
            if zero_start < zero_end {
                let destination_offset = usize::try_from(zero_start - page_address)
                    .map_err(|_| LoadError::InvalidFormat)?;
                let zero_len =
                    usize::try_from(zero_end - zero_start).map_err(|_| LoadError::InvalidFormat)?;
                let kernel_page =
                    petroleum::common::memory::physical_to_virtual(physical_address) as *mut u8;
                unsafe {
                    ptr::write_bytes(kernel_page.add(destination_offset), 0, zero_len);
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
                .then(|| load_bias + ph.p_vaddr + (elf.header.e_phoff - ph.p_offset))
        })
        .ok_or(LoadError::InvalidFormat)?;

    Ok(LinuxImageLayout {
        initial_break: align_up(image_end).ok_or(LoadError::InvalidFormat)?,
        phdr,
        phent: u64::from(elf.header.e_phentsize),
        phnum: u64::from(elf.header.e_phnum),
        entry: load_bias
            .checked_add(elf.entry)
            .ok_or(LoadError::InvalidFormat)?,
        at_base: 0,
    })
}

fn initialize_linux_stack(
    page_table: &mut ProcessPageTable,
    argv: &[&str],
    envp: &[&str],
    layout: LinuxImageLayout,
) -> Result<u64, LoadError> {
    let mut changes = Vec::new();
    let result =
        initialize_linux_stack_transaction(page_table, argv, envp, layout, &mut changes, false);
    if result.is_err() {
        rollback_page_changes(page_table, &changes);
    }
    result
}

fn reserve_linux_brk(
    _page_table: &mut ProcessPageTable,
    initial_break: u64,
    _changes: &mut Vec<PageChange>,
) -> Result<(), LoadError> {
    let end = align_up(initial_break)
        .ok_or(LoadError::InvalidFormat)?
        .checked_add(LINUX_BRK_RESERVE_SIZE)
        .ok_or(LoadError::InvalidFormat)?;
    if !petroleum::is_user_address(
        x86_64::VirtAddr::try_new(end - 1).map_err(|_| LoadError::InvalidFormat)?,
    ) {
        return Err(LoadError::UnsupportedArchitecture);
    }
    Ok(())
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn hardware_random_u64() -> Option<u64> {
    unsafe {
        let extended = core::arch::x86_64::__cpuid_count(7, 0);
        if (extended.ebx & (1 << 18)) != 0 {
            for _ in 0..10 {
                let mut value = 0;
                if core::arch::x86_64::_rdseed64_step(&mut value) == 1 {
                    return Some(value);
                }
            }
        }

        let features = core::arch::x86_64::__cpuid(1);
        if (features.ecx & (1 << 30)) != 0 {
            for _ in 0..10 {
                let mut value = 0;
                if core::arch::x86_64::_rdrand64_step(&mut value) == 1 {
                    return Some(value);
                }
            }
        }
    }
    None
}

pub(crate) fn linux_stack_random() -> [u8; 16] {
    let local = 0u8;
    let fallback_seed = unsafe { core::arch::x86_64::_rdtsc() }
        ^ crate::interrupts::TICK_COUNTER
            .load(Ordering::Relaxed)
            .rotate_left(17)
        ^ (&local as *const u8 as u64).rotate_left(31)
        ^ x86_64::registers::control::Cr3::read()
            .0
            .start_address()
            .as_u64()
            .rotate_left(47);
    let first = hardware_random_u64().unwrap_or_else(|| splitmix64(fallback_seed));
    let second =
        hardware_random_u64().unwrap_or_else(|| splitmix64(fallback_seed ^ first.rotate_left(23)));
    let mut random = [0u8; 16];
    random[..8].copy_from_slice(&first.to_ne_bytes());
    random[8..].copy_from_slice(&second.to_ne_bytes());
    random
}

fn initialize_linux_stack_transaction(
    page_table: &mut ProcessPageTable,
    argv: &[&str],
    envp: &[&str],
    layout: LinuxImageLayout,
    changes: &mut Vec<PageChange>,
    replace_existing: bool,
) -> Result<u64, LoadError> {
    if argv.is_empty() {
        return Err(LoadError::InvalidFormat);
    }

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
            if !replace_existing {
                return Err(LoadError::AddressAlreadyMapped);
            }
            let physical = PageTableHelper::translate_address(page_table, page_address as usize)
                .map_err(|_| LoadError::MappingFailed)?;
            unsafe {
                ptr::write_bytes(
                    petroleum::common::memory::physical_to_virtual(physical) as *mut u8,
                    0,
                    PAGE_SIZE as usize,
                );
            }
            PageTableHelper::set_page_flags(page_table, page_address as usize, stack_flags)
                .map_err(|_| LoadError::MappingFailed)?;
            page_address += PAGE_SIZE;
            continue;
        }
        map_zeroed_page(page_table, page_address, stack_flags)?;
        changes.push(PageChange::New {
            address: page_address,
        });
        page_address += PAGE_SIZE;
    }
    // Reassert the complete writable path after all stack pages exist. This
    // also covers a page-table hierarchy inherited or pre-created before the
    // Linux image was loaded. Do this for every leaf: the initial RSP is near
    // the top of the stack, while checking only stack_bottom would not repair
    // a read-only leaf on the active page.
    let mut writable_page = stack_bottom;
    while writable_page < LINUX_STACK_TOP {
        ensure_user_writable_path(page_table, writable_page)?;
        writable_page += PAGE_SIZE;
    }

    let mut cursor = LINUX_STACK_TOP;
    // Place argument and environment strings at the top of the stack.  Keep
    // their user addresses so the pointer vectors below can be emitted after
    // the auxiliary vector, exactly as a normal Linux process expects.
    let mut argv_addresses = Vec::new();
    for value in argv.iter().rev() {
        let bytes = value.as_bytes();
        cursor = cursor
            .checked_sub(bytes.len() as u64 + 1)
            .ok_or(LoadError::InvalidFormat)?;
        let address = cursor;
        write_process_bytes(page_table, address, bytes)?;
        write_process_bytes(page_table, address + bytes.len() as u64, &[0])?;
        argv_addresses.push(address);
    }
    argv_addresses.reverse();

    let mut env_addresses = Vec::new();
    for value in envp.iter().rev() {
        let bytes = value.as_bytes();
        cursor = cursor
            .checked_sub(bytes.len() as u64 + 1)
            .ok_or(LoadError::InvalidFormat)?;
        let address = cursor;
        write_process_bytes(page_table, address, bytes)?;
        write_process_bytes(page_table, address + bytes.len() as u64, &[0])?;
        env_addresses.push(address);
    }
    env_addresses.reverse();

    let argv0 = argv_addresses[0];

    cursor = cursor.checked_sub(16).ok_or(LoadError::InvalidFormat)?;
    let random_address = cursor;
    let random = linux_stack_random();
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

    let mut words = Vec::with_capacity(1 + argv_addresses.len() + 1 + env_addresses.len() + 1 + 18);
    words.push(argv_addresses.len() as u64);
    words.extend_from_slice(&argv_addresses);
    words.push(0);
    words.extend_from_slice(&env_addresses);
    words.push(0);
    words.extend_from_slice(&[
        AT_PHDR,
        layout.phdr,
        AT_PHENT,
        layout.phent,
        AT_PHNUM,
        layout.phnum,
        AT_PAGESZ,
        PAGE_SIZE,
        AT_BASE,
        layout.at_base,
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
    ]);
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
pub fn load_program(image_data: &[u8], name: &str) -> Result<process::ProcessId, LoadError> {
    load_program_inner(image_data, name, &[], &[], false, None, None, None, false)
}

/// Load a native program as a child of the requesting process.
pub fn load_program_with_parent(
    image_data: &[u8],
    name: &str,
    parent_id: process::ProcessId,
) -> Result<process::ProcessId, LoadError> {
    load_program_inner(
        image_data,
        name,
        &[],
        &[],
        false,
        Some(parent_id),
        None,
        None,
        false,
    )
}

/// Load a native program with independent lifecycle relationships.
pub fn load_program_with_relationships(
    image_data: &[u8],
    name: &str,
    parent_id: process::ProcessId,
    supervisor_id: Option<process::ProcessId>,
    terminal_id: Option<u64>,
) -> Result<process::ProcessId, LoadError> {
    load_program_with_relationships_and_authorization(
        image_data,
        name,
        parent_id,
        supervisor_id,
        terminal_id,
        false,
    )
}

/// Load a native program with lifecycle relationships and an explicit
/// kernel-issued Nozzle authorization.
pub fn load_program_with_relationships_and_authorization(
    image_data: &[u8],
    name: &str,
    parent_id: process::ProcessId,
    supervisor_id: Option<process::ProcessId>,
    terminal_id: Option<u64>,
    nozzle_authorized: bool,
) -> Result<process::ProcessId, LoadError> {
    load_program_inner(
        image_data,
        name,
        &[],
        &[],
        false,
        Some(parent_id),
        supervisor_id,
        terminal_id,
        nozzle_authorized,
    )
}

/// Load a program, optionally with Linux ABI emulation.
pub fn load_program_with_runtime(
    image_data: &[u8],
    name: &str,
    is_linux: bool,
) -> Result<process::ProcessId, LoadError> {
    let argv = [name];
    load_program_with_runtime_args(image_data, name, &argv, &[], is_linux)
}

/// Load a program with an explicit Linux argv/envp stack.
pub fn load_program_with_runtime_args(
    image_data: &[u8],
    name: &str,
    argv: &[&str],
    envp: &[&str],
    is_linux: bool,
) -> Result<process::ProcessId, LoadError> {
    load_program_inner(
        image_data, name, argv, envp, is_linux, None, None, None, false,
    )
}

/// Replace the executable image in an existing Linux process address space.
///
/// This is used by the Linux `execve` syscall.  It deliberately shares the
/// same PT_INTERP/ET_DYN path as the initial process launch so a dynamically
/// linked BusyBox applet does not take a separate, less complete loader path.
pub fn load_linux_image_for_exec(
    page_table: &mut ProcessPageTable,
    image_data: &[u8],
    argv: &[&str],
    envp: &[&str],
) -> Result<(u64, u64, u64), LoadError> {
    let elf = goblin::elf::Elf::parse(image_data).map_err(|_| LoadError::InvalidFormat)?;
    if !matches!(
        elf.header.e_type,
        goblin::elf::header::ET_EXEC | goblin::elf::header::ET_DYN
    ) {
        return Err(LoadError::NotExecutable);
    }
    if elf.header.e_machine != goblin::elf::header::EM_X86_64 {
        return Err(LoadError::UnsupportedArchitecture);
    }

    let main_load_bias = if elf.header.e_type == goblin::elf::header::ET_DYN {
        PROGRAM_LOAD_BASE
    } else {
        0
    };
    let interpreter_data = elf
        .interpreter
        .map(|path| crate::fs::read_entire_file(path).map_err(|_| LoadError::FileNotFound))
        .transpose()?;
    let mut changes = Vec::new();
    let result = (|| {
        let main_layout = load_elf_segments_transaction(
            page_table,
            &elf,
            image_data,
            main_load_bias,
            &mut changes,
        )?;
        let mut layout = main_layout;
        let start_entry = if let Some(interpreter_data) = interpreter_data.as_deref() {
            let interpreter =
                goblin::elf::Elf::parse(interpreter_data).map_err(|_| LoadError::InvalidFormat)?;
            if interpreter.header.e_type != goblin::elf::header::ET_DYN
                || interpreter.header.e_machine != goblin::elf::header::EM_X86_64
                || interpreter.interpreter.is_some()
            {
                return Err(LoadError::NotExecutable);
            }
            let interpreter_layout = load_elf_segments_transaction(
                page_table,
                &interpreter,
                interpreter_data,
                DYNAMIC_LINKER_BASE,
                &mut changes,
            )?;
            layout.at_base = DYNAMIC_LINKER_BASE;
            interpreter_layout.entry
        } else {
            layout.entry
        };

        reserve_linux_brk(page_table, layout.initial_break, &mut changes)?;
        let rsp =
            initialize_linux_stack_transaction(page_table, argv, envp, layout, &mut changes, true)?;
        Ok((start_entry, rsp, layout.initial_break))
    })();
    match result {
        Ok(values) => {
            // Execve is reached only after Linux fork has copied the child's
            // user leaves, so replaced frames belong exclusively to this
            // address space and can be released after commit.
            release_replaced_frames(&changes);
            Ok(values)
        }
        Err(error) => {
            rollback_page_changes(page_table, &changes);
            Err(error)
        }
    }
}

fn load_program_inner(
    image_data: &[u8],
    name: &str,
    argv: &[&str],
    envp: &[&str],
    is_linux: bool,
    parent_id: Option<process::ProcessId>,
    supervisor_id: Option<process::ProcessId>,
    terminal_id: Option<u64>,
    nozzle_authorized: bool,
) -> Result<process::ProcessId, LoadError> {
    crate::klog_fmt!(
        "[LINUX-DIAG] elf parse begin name={} bytes={} linux={}\n",
        name,
        image_data.len(),
        is_linux
    );
    // Parse ELF using goblin
    let elf = goblin::elf::Elf::parse(image_data).map_err(|_| LoadError::InvalidFormat)?;
    crate::klog_fmt!(
        "[LINUX-DIAG] elf parse exit entry={:#x} phnum={} interp={}\n",
        elf.entry,
        elf.program_headers.len(),
        elf.interpreter.is_some()
    );

    if !matches!(
        elf.header.e_type,
        goblin::elf::header::ET_EXEC | goblin::elf::header::ET_DYN
    ) {
        return Err(LoadError::NotExecutable);
    }
    if elf.header.e_machine != goblin::elf::header::EM_X86_64 {
        return Err(LoadError::UnsupportedArchitecture);
    }
    let interpreter_data = if is_linux {
        elf.interpreter
            .map(|path| crate::fs::read_entire_file(path).map_err(|_| LoadError::FileNotFound))
            .transpose()?
    } else {
        None
    };
    let main_load_bias = if elf.header.e_type == goblin::elf::header::ET_DYN {
        PROGRAM_LOAD_BASE
    } else {
        0
    };
    let interpreter_entry = interpreter_data
        .as_deref()
        .map(|data| {
            let interpreter =
                goblin::elf::Elf::parse(data).map_err(|_| LoadError::InvalidFormat)?;
            if interpreter.header.e_type != goblin::elf::header::ET_DYN
                || interpreter.header.e_machine != goblin::elf::header::EM_X86_64
                || interpreter.interpreter.is_some()
            {
                return Err(LoadError::NotExecutable);
            }
            DYNAMIC_LINKER_BASE
                .checked_add(interpreter.header.e_entry)
                .ok_or(LoadError::InvalidFormat)
        })
        .transpose()?;
    let entry = interpreter_entry.unwrap_or(
        main_load_bias
            .checked_add(elf.header.e_entry)
            .ok_or(LoadError::InvalidFormat)?,
    );
    let entry_point_address =
        x86_64::VirtAddr::try_new(entry).map_err(|_| LoadError::InvalidFormat)?;
    if !petroleum::is_user_address(entry_point_address) {
        return Err(LoadError::UnsupportedArchitecture);
    }

    // Create process with the loaded program (user mode)
    let pid = process::create_process_with_relationships_and_authorization(
        name,
        entry_point_address,
        true,
        parent_id,
        supervisor_id,
        terminal_id,
        nozzle_authorized,
    )?;
    crate::klog_fmt!(
        "[LINUX-DIAG] process created pid={} entry={:#x}\n",
        pid.0,
        entry
    );

    let load_result = process::SCHEDULER
        .with_process(pid, |p| {
            let mut changes = Vec::new();
            let main_layout = {
                let process_page_table = p.page_table.as_mut().ok_or(LoadError::InvalidFormat)?;
                crate::klog_fmt!("[LINUX-DIAG] segments begin pid={}\n", pid.0);
                load_elf_segments_transaction(
                    process_page_table,
                    &elf,
                    image_data,
                    main_load_bias,
                    &mut changes,
                )?
            };
            let mut loaded = LoadedLinuxImage {
                layout: main_layout,
                changes,
            };
            if let Some(interpreter_data) = interpreter_data.as_deref() {
                let interpreter = goblin::elf::Elf::parse(interpreter_data)
                    .map_err(|_| LoadError::InvalidFormat)?;
                let interpreter_layout = {
                    let process_page_table =
                        p.page_table.as_mut().ok_or(LoadError::InvalidFormat)?;
                    load_elf_segments_transaction(
                        process_page_table,
                        &interpreter,
                        interpreter_data,
                        DYNAMIC_LINKER_BASE,
                        &mut loaded.changes,
                    )?
                };
                loaded.layout.at_base = DYNAMIC_LINKER_BASE;
                crate::klog_fmt!(
                    "[LINUX-DIAG] dynamic linker loaded base={:#x} entry={:#x}\n",
                    DYNAMIC_LINKER_BASE,
                    interpreter_layout.entry
                );
            }
            crate::klog_fmt!(
                "[LINUX-DIAG] segments exit pid={} break={:#x}\n",
                pid.0,
                loaded.layout.initial_break
            );
            if is_linux {
                let reserve_result = {
                    let process_page_table =
                        p.page_table.as_mut().ok_or(LoadError::InvalidFormat)?;
                    reserve_linux_brk(
                        process_page_table,
                        loaded.layout.initial_break,
                        &mut loaded.changes,
                    )
                };
                if let Err(error) = reserve_result {
                    if let Some(process_page_table) = p.page_table.as_mut() {
                        rollback_page_changes(process_page_table, &loaded.changes);
                    }
                    return Err(error);
                }

                let stack_result = {
                    let process_page_table =
                        p.page_table.as_mut().ok_or(LoadError::InvalidFormat)?;
                    crate::klog_fmt!("[LINUX-DIAG] stack begin pid={}\n", pid.0);
                    initialize_linux_stack(process_page_table, argv, envp, loaded.layout)
                };
                let rsp = match stack_result {
                    Ok(rsp) => rsp,
                    Err(error) => {
                        if let Some(process_page_table) = p.page_table.as_mut() {
                            rollback_page_changes(process_page_table, &loaded.changes);
                        }
                        return Err(error);
                    }
                };

                // `create_process` supplies the legacy native-user stack from
                // the kernel heap.  Linux processes use the mapped lower-half
                // stack above, so release the unused allocation.
                if let Some(old_stack_base) = p
                    .user_stack
                    .as_u64()
                    .checked_sub(crate::heap::KERNEL_STACK_SIZE as u64)
                    .filter(|&base| {
                        base != 0
                            && petroleum::common::memory::is_allocator_related_address(
                                base as usize,
                            )
                    })
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
                p.context.registers.rsp = rsp;
                // Linux starts with maskable interrupts enabled. Keeping IF
                // clear here makes a user process run without timer ticks,
                // which also freezes Klog Live immediately after iretq.
                p.context.rflags = 0x202;
                crate::klog_fmt!("[LINUX-DIAG] stack exit pid={} rsp={:#x}\n", pid.0, rsp);
                let runtime =
                    crate::solvent_linux::LinuxRuntime::new(p.id.0, loaded.layout.initial_break);
                p.dispatch_mode = Some(crate::solvent_linux::DispatchMode::Linux(
                    alloc::boxed::Box::new(runtime),
                ));
            }
            Ok::<(), LoadError>(())
        })
        .ok_or(LoadError::InvalidFormat)
        .and_then(|result| result);

    if let Err(error) = load_result {
        abort_created_process(pid);
        return Err(error);
    }

    Ok(pid)
}

fn abort_created_process(pid: process::ProcessId) {
    process::SCHEDULER.with_process(pid, |p| {
        if let Some(user_stack_base) = p
            .user_stack
            .as_u64()
            .checked_sub(crate::heap::KERNEL_STACK_SIZE as u64)
            .filter(|&base| {
                base != 0 && petroleum::common::memory::is_allocator_related_address(base as usize)
            })
        {
            let stack_layout =
                core::alloc::Layout::from_size_align(crate::heap::KERNEL_STACK_SIZE, 16)
                    .expect("constant user stack layout");
            unsafe {
                petroleum::common::memory::deallocate_layout(
                    user_stack_base as *mut u8,
                    stack_layout,
                );
            }
            p.user_stack = x86_64::VirtAddr::new(0);
        }
    });
    process::terminate_process(pid, -1);
    process::SCHEDULER.cleanup();
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

    #[test]
    fn merged_load_page_never_becomes_writable_and_executable() {
        let executable = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        let writable = PageTableFlags::PRESENT
            | PageTableFlags::USER_ACCESSIBLE
            | PageTableFlags::WRITABLE
            | PageTableFlags::NO_EXECUTE;
        let merged = merge_segment_page_flags(executable, writable);

        assert!(!merged.contains(PageTableFlags::NO_EXECUTE));
        assert!(!merged.contains(PageTableFlags::WRITABLE));
    }

    #[test]
    fn non_executable_load_page_stays_nx() {
        let read_only =
            PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE;
        let writable = read_only | PageTableFlags::WRITABLE;
        let merged = merge_segment_page_flags(read_only, writable);

        assert!(merged.contains(PageTableFlags::NO_EXECUTE));
        assert!(merged.contains(PageTableFlags::WRITABLE));
    }
}
