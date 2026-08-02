// Linux memory syscall implementations
use super::numbers::*;
use super::runtime::{LinuxRuntime, errno_code};

use petroleum::page_table::process::ProcessPageTable;
use petroleum::page_table::types::PageTableHelper;
use x86_64::VirtAddr;
use x86_64::structures::paging::Size4KiB;
use x86_64::structures::paging::{FrameAllocator as X86FrameAllocator, PageTableFlags};

const PAGE_SIZE: u64 = 4096;
const PAGE_MASK: u64 = PAGE_SIZE - 1;
const MAX_LINUX_MEMORY: u64 = 128 * 1024 * 1024;
const MAX_LINUX_BRK: u64 = 64 * 1024 * 1024;
const USER_ADDRESS_LIMIT: u64 = 0x0000_8000_0000_0000;
const DEFAULT_MMAP_BASE: u64 = 0x0000_0001_0000_0000;
const VDSO_SIZE: u64 = PAGE_SIZE;
const MAX_MMAP_PROBES: usize = (MAX_LINUX_MEMORY / PAGE_SIZE) as usize * 4;

/// Per-process virtual memory region tracked for mmap/munmap.
#[derive(Clone, Copy)]
pub struct LinuxMmapRegion {
    pub addr: u64,
    pub size: u64,
    pub prot: i32,
    pub flags: i32,
}

/// Validate and page-align a user virtual address range without touching it.
///
/// This is deliberately separate from `UserSlice`: an mmap range is not
/// mapped yet, so it cannot be validated as a user buffer.  It still must be
/// canonical, entirely below the user/kernel split, and free of arithmetic
/// overflow.
fn checked_page_range(
    addr: u64,
    length: u64,
    require_aligned_addr: bool,
) -> Result<(u64, u64), i32> {
    if length == 0 || length > MAX_LINUX_MEMORY {
        return Err(EINVAL);
    }
    if require_aligned_addr && (addr & PAGE_MASK) != 0 {
        return Err(EINVAL);
    }

    let start = if require_aligned_addr {
        addr
    } else {
        addr & !PAGE_MASK
    };
    let last = addr.checked_add(length - 1).ok_or(EINVAL)?;
    let end = last.checked_add(PAGE_MASK).ok_or(EINVAL)? & !PAGE_MASK;
    let size = end.checked_sub(start).ok_or(EINVAL)?;

    if size == 0 || size > MAX_LINUX_MEMORY || end > USER_ADDRESS_LIMIT {
        return Err(EINVAL);
    }
    let start_va = VirtAddr::try_new(start).map_err(|_| EINVAL)?;
    let end_va = VirtAddr::try_new(end - 1).map_err(|_| EINVAL)?;
    if !petroleum::is_user_address(start_va) || !petroleum::is_user_address(end_va) {
        return Err(EINVAL);
    }
    Ok((start, size))
}

fn ranges_overlap(left_addr: u64, left_size: u64, right_addr: u64, right_size: u64) -> bool {
    let Some(left_end) = left_addr.checked_add(left_size) else {
        return true;
    };
    let Some(right_end) = right_addr.checked_add(right_size) else {
        return true;
    };
    left_addr < right_end && right_addr < left_end
}

fn tracked_range_overlaps(rt: &LinuxRuntime, addr: u64, size: u64) -> bool {
    rt.mmap_regions
        .iter()
        .any(|region| ranges_overlap(region.addr, region.size, addr, size))
}

fn tracked_range_contains(rt: &LinuxRuntime, addr: u64, size: u64) -> bool {
    let Some(end) = addr.checked_add(size) else {
        return false;
    };
    rt.mmap_regions.iter().any(|region| {
        region
            .addr
            .checked_add(region.size)
            .is_some_and(|region_end| addr >= region.addr && end <= region_end)
    })
}

fn range_is_mapped(page_table: &ProcessPageTable, addr: u64, size: u64) -> bool {
    let pages = (size / PAGE_SIZE) as usize;
    (0..pages).any(|index| {
        let Some(page) = addr.checked_add(index as u64 * PAGE_SIZE) else {
            return true;
        };
        page_table.translate_address(page as usize).is_ok()
    })
}

fn range_is_owned_user_memory(page_table: &ProcessPageTable, addr: u64, size: u64) -> bool {
    let pages = (size / PAGE_SIZE) as usize;
    (0..pages).all(|index| {
        let Some(page) = addr.checked_add(index as u64 * PAGE_SIZE) else {
            return false;
        };
        let Ok(flags) = page_table.get_page_flags(page as usize) else {
            return false;
        };
        flags.contains(PageTableFlags::USER_ACCESSIBLE)
    })
}

fn overlaps_reserved_user_mapping(addr: u64, size: u64) -> bool {
    ranges_overlap(addr, size, petroleum::vdso::VDSO_USER_BASE, VDSO_SIZE)
        || ranges_overlap(
            addr,
            size,
            crate::loader::DYNAMIC_LINKER_BASE,
            crate::loader::DYNAMIC_LINKER_RESERVE_SIZE,
        )
}

/// The VDSO is an immutable kernel-owned mapping.  The dynamic linker is
/// reserved from mmap/munmap placement, but its already-mapped ELF segments
/// must remain available to the linker itself for RELRO mprotect calls after
/// relocation.
fn overlaps_immutable_user_mapping(addr: u64, size: u64) -> bool {
    ranges_overlap(addr, size, petroleum::vdso::VDSO_USER_BASE, VDSO_SIZE)
}

fn range_is_fully_mapped(page_table: &ProcessPageTable, addr: u64, size: u64) -> bool {
    let pages = (size / PAGE_SIZE) as usize;
    (0..pages).all(|index| {
        let Some(page) = addr.checked_add(index as u64 * PAGE_SIZE) else {
            return false;
        };
        page_table.translate_address(page as usize).is_ok()
    })
}

fn find_free_anon_region(
    rt: &LinuxRuntime,
    page_table: &ProcessPageTable,
    size: u64,
    start: u64,
) -> u64 {
    let mut candidate = (start + PAGE_MASK) & !PAGE_MASK;
    for _ in 0..MAX_MMAP_PROBES {
        if candidate.checked_add(size).is_none()
            || candidate.checked_add(size).unwrap() > USER_ADDRESS_LIMIT
        {
            return 0;
        }

        if overlaps_reserved_user_mapping(candidate, size) {
            let linker_end = crate::loader::DYNAMIC_LINKER_BASE
                .saturating_add(crate::loader::DYNAMIC_LINKER_RESERVE_SIZE);
            candidate = if ranges_overlap(
                candidate,
                size,
                crate::loader::DYNAMIC_LINKER_BASE,
                crate::loader::DYNAMIC_LINKER_RESERVE_SIZE,
            ) {
                linker_end
            } else {
                petroleum::vdso::VDSO_USER_BASE.saturating_add(VDSO_SIZE)
            };
            continue;
        }

        if tracked_range_overlaps(rt, candidate, size) {
            candidate = rt
                .mmap_regions
                .iter()
                .filter(|region| ranges_overlap(region.addr, region.size, candidate, size))
                .filter_map(|region| region.addr.checked_add(region.size))
                .max()
                .unwrap_or(USER_ADDRESS_LIMIT);
            let Some(aligned) = candidate.checked_add(PAGE_MASK) else {
                return 0;
            };
            candidate = aligned & !PAGE_MASK;
            continue;
        }

        if range_is_mapped(page_table, candidate, size) {
            // Advance past the first mapped page.  The next iteration also
            // checks tracked ranges, so a collision cannot be bypassed by
            // choosing an address supplied by the caller.
            candidate = candidate.saturating_add(PAGE_SIZE);
            continue;
        }
        return candidate;
    }
    0
}

fn with_current_page_table<R>(operation: impl FnOnce(&mut ProcessPageTable) -> R) -> R {
    let (pml4_frame, _) = x86_64::registers::control::Cr3::read();
    let physical_offset =
        VirtAddr::new(petroleum::common::memory::get_physical_memory_offset() as u64);
    let pml4 = physical_offset + pml4_frame.start_address().as_u64();
    let mapper = unsafe {
        x86_64::structures::paging::OffsetPageTable::new(
            &mut *(pml4.as_mut_ptr::<x86_64::structures::paging::PageTable>()),
            physical_offset,
        )
    };
    let mut page_table = ProcessPageTable::new_with_frame(pml4_frame);
    page_table.mapper = Some(mapper);
    page_table.initialized = true;
    operation(&mut page_table)
}

fn unmap_and_free(page_table: &mut ProcessPageTable, virtual_addr: usize) -> Result<(), ()> {
    let frame = page_table.unmap_page(virtual_addr).map_err(|_| ())?;
    let frame_alloc = unsafe { petroleum::page_table::constants::get_frame_allocator_mut() };
    frame_alloc.free_frame(frame);
    Ok(())
}

fn track_region(
    rt: &mut LinuxRuntime,
    addr: u64,
    size: u64,
    prot: i32,
    flags: i32,
) -> Result<(), i32> {
    rt.mmap_regions
        .push(LinuxMmapRegion {
            addr,
            size,
            prot,
            flags,
        })
        .map_err(|_| ENOMEM)
}

fn remove_region(rt: &mut LinuxRuntime, addr: u64, size: u64) -> bool {
    if let Some(pos) = rt
        .mmap_regions
        .iter()
        .position(|region| region.addr == addr && region.size == size)
    {
        rt.mmap_regions.remove(pos);
        true
    } else {
        false
    }
}

fn trim_overlapping_regions(rt: &mut LinuxRuntime, addr: u64, size: u64) -> Result<(), i32> {
    let end = addr.checked_add(size).ok_or(ENOMEM)?;
    let mut remaining = heapless::Vec::<LinuxMmapRegion, 64>::new();
    for region in rt.mmap_regions.iter().copied() {
        let region_end = region.addr.checked_add(region.size).ok_or(ENOMEM)?;
        if !ranges_overlap(region.addr, region.size, addr, size) {
            remaining.push(region).map_err(|_| ENOMEM)?;
            continue;
        }
        if region.addr < addr {
            remaining
                .push(LinuxMmapRegion {
                    addr: region.addr,
                    size: addr - region.addr,
                    ..region
                })
                .map_err(|_| ENOMEM)?;
        }
        if end < region_end {
            remaining
                .push(LinuxMmapRegion {
                    addr: end,
                    size: region_end - end,
                    ..region
                })
                .map_err(|_| ENOMEM)?;
        }
    }
    rt.mmap_regions = remaining;
    Ok(())
}

/// Release mmap-created regions before an execve image is installed.  The
/// executable, stack, brk reservation, and VDSO are intentionally not in
/// `mmap_regions`, so this cannot tear down the process's fixed ABI mappings.
pub fn reset_mmap_regions(rt: &mut LinuxRuntime) {
    let regions = rt
        .mmap_regions
        .iter()
        .map(|region| (region.addr, region.size))
        .collect::<alloc::vec::Vec<_>>();
    with_current_page_table(|page_table| {
        for (addr, size) in regions {
            for index in 0..(size / PAGE_SIZE) {
                let page = addr + index * PAGE_SIZE;
                let _ = unmap_and_free(page_table, page as usize);
            }
        }
    });
    rt.mmap_regions.clear();
}

fn read_vfs_at(vfs_fd: u32, offset: u64, output: &mut [u8]) -> Result<usize, ()> {
    let saved = crate::contexts::vfs::position(vfs_fd).map_err(|_| ())?;
    crate::contexts::vfs::seek(vfs_fd, offset).map_err(|_| ())?;
    let result = crate::contexts::vfs::read(vfs_fd, output).map_err(|_| ());
    let _ = crate::contexts::vfs::seek(vfs_fd, saved);
    result
}

pub fn sys_mmap(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let addr_hint = args[0];
    let length = args[1];
    let prot = args[2] as i32;
    let flags = args[3] as i32;
    let offset = args[5];

    #[cfg(linux_busybox_smoke)]
    petroleum::serial::serial_log(format_args!(
        "[linux-mmap] request hint={addr_hint:#x} len={length:#x} prot={prot:#x} flags={flags:#x} fd={} off={offset:#x}\n",
        args[4] as i32
    ));

    let allowed_prot = PROT_READ | PROT_WRITE | PROT_EXEC;
    if (prot & !allowed_prot) != 0 {
        return errno_code(EINVAL);
    }
    if (flags & (MAP_PRIVATE | MAP_SHARED)) == 0
        || (flags & (MAP_PRIVATE | MAP_SHARED)) == (MAP_PRIVATE | MAP_SHARED)
    {
        return errno_code(EINVAL);
    }
    if length == 0 {
        return errno_code(EINVAL);
    }

    let anon = (flags & MAP_ANONYMOUS) != 0;
    if (offset & PAGE_MASK) != 0 {
        return errno_code(EINVAL);
    }
    let file_vfs_fd = if anon {
        None
    } else {
        if (flags & MAP_SHARED) != 0 && (prot & PROT_WRITE) != 0 {
            return errno_code(EINVAL);
        }
        let fd = args[4] as i32;
        let Some(desc) = rt.fd_table.get(fd) else {
            return errno_code(EBADF);
        };
        if desc.vfs_fd == 0 || desc.pipe.is_some() {
            return errno_code(EBADF);
        }
        Some(desc.vfs_fd)
    };

    let (_, aligned_len) = match checked_page_range(0, length, false) {
        Ok(range) => range,
        Err(error) => return errno_code(error),
    };

    let fixed = (flags & MAP_FIXED) != 0;
    let hint = if fixed {
        match checked_page_range(addr_hint, aligned_len, true) {
            Ok((addr, _)) => addr,
            Err(error) => return errno_code(error),
        }
    } else if addr_hint == 0 {
        DEFAULT_MMAP_BASE
    } else {
        // A hint is still an address supplied by an untrusted process.  Reject
        // non-canonical/kernel ranges before using or aligning it.
        match checked_page_range(addr_hint, aligned_len, false) {
            Ok((addr, _)) => addr,
            Err(error) => return errno_code(error),
        }
    };

    let mapped = with_current_page_table(|page_table| {
        #[cfg(linux_musl_smoke)]
        petroleum::serial::serial_log(format_args!("[linux-smoke] mmap page table acquired\n"));
        let addr = if fixed {
            if overlaps_reserved_user_mapping(hint, aligned_len) {
                return Err(EEXIST);
            }
            if range_is_mapped(page_table, hint, aligned_len) {
                // ELF loaders use MAP_FIXED to replace the first, broadly
                // mapped file segment with subsequent segment protections.
                // Permit that only for a range already owned by this mmap
                // runtime; arbitrary replacement of the main image, stack,
                // VDSO, or brk remains rejected.
                if !range_is_fully_mapped(page_table, hint, aligned_len)
                    || !range_is_owned_user_memory(page_table, hint, aligned_len)
                    || !tracked_range_contains(rt, hint, aligned_len)
                {
                    return Err(EEXIST);
                }
                #[cfg(linux_busybox_smoke)]
                petroleum::serial::serial_log(format_args!(
                    "[linux-mmap] replace tracked addr={hint:#x} len={aligned_len:#x}\n"
                ));
                for index in 0..(aligned_len / PAGE_SIZE) {
                    let page = hint + index * PAGE_SIZE;
                    if unmap_and_free(page_table, page as usize).is_err() {
                        return Err(EEXIST);
                    }
                }
                trim_overlapping_regions(rt, hint, aligned_len)?;
            }
            hint
        } else {
            find_free_anon_region(rt, page_table, aligned_len, hint)
        };
        #[cfg(linux_musl_smoke)]
        petroleum::serial::serial_log(format_args!(
            "[linux-smoke] mmap selected {addr:#x}, length {aligned_len:#x}\n"
        ));
        if addr == 0 {
            return Err(ENOMEM);
        }

        let num_pages = (aligned_len / PAGE_SIZE) as usize;
        let frame_alloc = unsafe { petroleum::page_table::constants::get_frame_allocator_mut() };
        let mut mapped_pages = 0usize;
        let mut page_flags =
            PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE;
        if (prot & PROT_WRITE) != 0 {
            page_flags |= PageTableFlags::WRITABLE;
        }
        if (prot & PROT_EXEC) != 0 {
            page_flags.remove(PageTableFlags::NO_EXECUTE);
        }

        for index in 0..num_pages {
            let page_vaddr = addr + index as u64 * PAGE_SIZE;
            #[cfg(linux_musl_smoke)]
            petroleum::serial::serial_log(format_args!(
                "[linux-smoke] mmap mapping page {index} at {page_vaddr:#x}\n"
            ));
            let frame = match X86FrameAllocator::<Size4KiB>::allocate_frame(frame_alloc) {
                Some(frame) => frame,
                None => {
                    for mapped in 0..mapped_pages {
                        let page = (addr + mapped as u64 * PAGE_SIZE) as usize;
                        let _ = unmap_and_free(page_table, page);
                    }
                    return Err(ENOMEM);
                }
            };
            unsafe {
                core::ptr::write_bytes(
                    petroleum::common::memory::physical_to_virtual(
                        frame.start_address().as_u64() as usize
                    ) as *mut u8,
                    0,
                    PAGE_SIZE as usize,
                );
            }

            if let Some(vfs_fd) = file_vfs_fd {
                let file_offset = offset.checked_add(index as u64 * PAGE_SIZE).ok_or(EINVAL)?;
                let destination = unsafe {
                    core::slice::from_raw_parts_mut(
                        petroleum::common::memory::physical_to_virtual(
                            frame.start_address().as_u64() as usize,
                        ) as *mut u8,
                        PAGE_SIZE as usize,
                    )
                };
                if read_vfs_at(vfs_fd, file_offset, destination).is_err() {
                    for mapped in 0..mapped_pages {
                        let page = (addr + mapped as u64 * PAGE_SIZE) as usize;
                        let _ = unmap_and_free(page_table, page);
                    }
                    frame_alloc.free_frame(frame);
                    return Err(EIO);
                }
            }

            if page_table
                .map_page(
                    page_vaddr as usize,
                    frame.start_address().as_u64() as usize,
                    page_flags,
                    frame_alloc,
                )
                .is_err()
            {
                frame_alloc.free_frame(frame);
                for mapped in 0..mapped_pages {
                    let page = (addr + mapped as u64 * PAGE_SIZE) as usize;
                    let _ = unmap_and_free(page_table, page);
                }
                return Err(ENOMEM);
            }
            mapped_pages += 1;
        }
        #[cfg(linux_musl_smoke)]
        petroleum::serial::serial_log(format_args!("[linux-smoke] mmap mapping done\n"));
        Ok(addr)
    });
    let addr = match mapped {
        Ok(addr) => addr,
        Err(error) => return errno_code(error),
    };

    #[cfg(linux_busybox_smoke)]
    petroleum::serial::serial_log(format_args!(
        "[linux-mmap] mapped addr={addr:#x} len={aligned_len:#x} file={:?}\n",
        file_vfs_fd
    ));

    #[cfg(linux_musl_smoke)]
    petroleum::serial::serial_log(format_args!("[linux-smoke] mmap tracking region\n"));
    if let Err(error) = track_region(rt, addr, aligned_len, prot, flags) {
        with_current_page_table(|page_table| {
            for index in 0..(aligned_len / PAGE_SIZE) {
                let page = (addr + index * PAGE_SIZE) as usize;
                let _ = unmap_and_free(page_table, page);
            }
        });
        return errno_code(error);
    }
    #[cfg(linux_musl_smoke)]
    petroleum::serial::serial_log(format_args!("[linux-smoke] mmap returning {addr:#x}\n"));
    addr
}

pub fn sys_munmap(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let addr = args[0];
    let length = args[1];
    let (aligned_addr, aligned_len) = match checked_page_range(addr, length, true) {
        Ok(range) => range,
        Err(error) => return errno_code(error),
    };
    if overlaps_reserved_user_mapping(aligned_addr, aligned_len) {
        return errno_code(EINVAL);
    }

    let result = with_current_page_table(|page_table| {
        if !range_is_owned_user_memory(page_table, aligned_addr, aligned_len) {
            return Err(EINVAL);
        }

        let pages = (aligned_len / PAGE_SIZE) as usize;
        for index in 0..pages {
            let page = aligned_addr + index as u64 * PAGE_SIZE;
            if unmap_and_free(page_table, page as usize).is_err() {
                return Err(EINVAL);
            }
        }
        Ok(())
    });
    if let Err(error) = result {
        return errno_code(error);
    }

    // Exact-region removal is sufficient for the mappings this layer creates.
    // If a caller unmaps a subrange, leave bookkeeping intact rather than
    // making a later mprotect operation less restrictive by accident.
    let _ = remove_region(rt, aligned_addr, aligned_len);
    0
}

pub fn sys_mprotect(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let addr = args[0];
    let length = args[1];
    let prot = args[2] as i32;
    let allowed_prot = PROT_READ | PROT_WRITE | PROT_EXEC;
    if (prot & !allowed_prot) != 0 {
        return errno_code(EINVAL);
    }
    let (aligned_addr, aligned_len) = match checked_page_range(addr, length, false) {
        Ok(range) => range,
        Err(error) => return errno_code(error),
    };
    // The dynamic linker lives in a reserved address range, but glibc must be
    // able to mprotect its mapped RELRO pages after relocation.  Only the
    // kernel-owned VDSO is immutable here; mmap/munmap continue to reject the
    // full reserved set above.
    if overlaps_immutable_user_mapping(aligned_addr, aligned_len) {
        return errno_code(EINVAL);
    }

    let mut page_flags = PageTableFlags::USER_ACCESSIBLE;
    if prot != PROT_NONE {
        page_flags |= PageTableFlags::PRESENT;
        if (prot & PROT_WRITE) != 0 {
            page_flags |= PageTableFlags::WRITABLE;
        }
        if (prot & PROT_EXEC) == 0 {
            page_flags |= PageTableFlags::NO_EXECUTE;
        }
    }

    let result = with_current_page_table(|page_table| {
        if !range_is_owned_user_memory(page_table, aligned_addr, aligned_len) {
            return Err(EINVAL);
        }

        let pages = (aligned_len / PAGE_SIZE) as usize;
        let mut original_flags = alloc::vec::Vec::with_capacity(pages);
        for index in 0..pages {
            let page = (aligned_addr + index as u64 * PAGE_SIZE) as usize;
            let flags = page_table
                .get_page_flags(page as usize)
                .map_err(|_| EINVAL)?;
            original_flags.push((page, flags));
        }

        for (index, (page, _)) in original_flags.iter().enumerate() {
            if page_table.set_page_flags(*page, page_flags).is_err() {
                for (restore_page, restore_flags) in original_flags.iter().take(index) {
                    let _ = page_table.set_page_flags(*restore_page, *restore_flags);
                }
                return Err(ENOMEM);
            }
        }
        Ok(())
    });
    if let Err(error) = result {
        return errno_code(error);
    }

    // Keep the runtime's metadata in sync when the range is an mmap region.
    if let Some(region) = rt
        .mmap_regions
        .iter_mut()
        .find(|region| region.addr == aligned_addr && region.size == aligned_len)
    {
        region.prot = prot;
    }
    0
}

pub fn sys_brk(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let new_brk = args[0];

    #[cfg(linux_busybox_smoke)]
    petroleum::serial::serial_log(format_args!(
        "[linux-brk] initial={:#x} current={:#x} request={new_brk:#x}\n",
        rt.initial_break, rt.program_break
    ));

    if new_brk == 0 {
        return rt.program_break;
    }

    if new_brk < rt.initial_break
        || new_brk >= USER_ADDRESS_LIMIT
        || new_brk - rt.initial_break > MAX_LINUX_BRK
    {
        return rt.program_break;
    }

    let old_break = rt.program_break;
    if new_brk > old_break {
        let start = old_break.saturating_add(PAGE_MASK) & !PAGE_MASK;
        let end = new_brk.saturating_add(PAGE_MASK) & !PAGE_MASK;
        let result = with_current_page_table(|page_table| {
            let flags = PageTableFlags::PRESENT
                | PageTableFlags::WRITABLE
                | PageTableFlags::USER_ACCESSIBLE
                | PageTableFlags::NO_EXECUTE;
            let frame_alloc =
                unsafe { petroleum::page_table::constants::get_frame_allocator_mut() };
            let mut mapped = alloc::vec::Vec::new();
            let mut address = start;
            while address < end {
                if page_table.translate_address(address as usize).is_err() {
                    let Some(frame) = X86FrameAllocator::<Size4KiB>::allocate_frame(frame_alloc)
                    else {
                        for old_page in mapped.drain(..) {
                            if let Ok(frame) = page_table.unmap_page(old_page as usize) {
                                frame_alloc.free_frame(frame);
                            }
                        }
                        return Err(ENOMEM);
                    };
                    unsafe {
                        core::ptr::write_bytes(
                            petroleum::common::memory::physical_to_virtual(
                                frame.start_address().as_u64() as usize,
                            ) as *mut u8,
                            0,
                            PAGE_SIZE as usize,
                        );
                    }
                    if page_table
                        .map_page(
                            address as usize,
                            frame.start_address().as_u64() as usize,
                            flags,
                            frame_alloc,
                        )
                        .is_err()
                    {
                        frame_alloc.free_frame(frame);
                        for old_page in mapped.drain(..) {
                            if let Ok(frame) = page_table.unmap_page(old_page as usize) {
                                frame_alloc.free_frame(frame);
                            }
                        }
                        return Err(ENOMEM);
                    }
                    mapped.push(address);
                }
                address += PAGE_SIZE;
            }
            Ok(())
        });
        if result.is_err() {
            return rt.program_break;
        }
    }
    rt.program_break = new_brk;
    new_brk
}

pub fn sys_mremap(_rt: &mut LinuxRuntime, _args: &[u64; 6]) -> u64 {
    errno_code(ENOSYS)
}

pub fn sys_madvise(_rt: &mut LinuxRuntime, _args: &[u64; 6]) -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_kernel_and_overflowing_ranges() {
        assert_eq!(
            checked_page_range(0x0000_8000_0000_0000, 4096, true),
            Err(EINVAL)
        );
        assert_eq!(checked_page_range(u64::MAX - 1, 4096, true), Err(EINVAL));
    }

    #[test]
    fn rounds_mprotect_ranges_but_requires_aligned_unmap() {
        assert_eq!(checked_page_range(0x1234, 1, false), Ok((0x1000, 4096)));
        assert_eq!(checked_page_range(0x1234, 4096, true), Err(EINVAL));
    }

    #[test]
    fn rejects_ranges_larger_than_the_compatibility_limit() {
        assert_eq!(
            checked_page_range(0x1000, MAX_LINUX_MEMORY + PAGE_SIZE, false),
            Err(EINVAL)
        );
    }

    #[test]
    fn linker_reservation_is_not_immutable_for_mprotect() {
        assert!(overlaps_reserved_user_mapping(
            crate::loader::DYNAMIC_LINKER_BASE,
            PAGE_SIZE
        ));
        assert!(!overlaps_immutable_user_mapping(
            crate::loader::DYNAMIC_LINKER_BASE,
            PAGE_SIZE
        ));
        assert!(overlaps_immutable_user_mapping(
            petroleum::vdso::VDSO_USER_BASE,
            PAGE_SIZE
        ));
    }
}
