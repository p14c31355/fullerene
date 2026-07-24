// Linux memory syscall implementations
use super::numbers::*;
use super::runtime::{LinuxRuntime, errno_code};

use petroleum::page_table::types::PageTableHelper;
use x86_64::VirtAddr;
use x86_64::structures::paging::Size4KiB;
use x86_64::structures::paging::{FrameAllocator as X86FrameAllocator, PageTableFlags};

const PAGE_SIZE: u64 = 4096;
const PAGE_MASK: u64 = PAGE_SIZE - 1;
const MAX_LINUX_MEMORY: u64 = 128 * 1024 * 1024;
const USER_ADDRESS_LIMIT: u64 = 0x0000_8000_0000_0000;
const DEFAULT_MMAP_BASE: u64 = 0x0000_0001_0000_0000;
const VDSO_SIZE: u64 = PAGE_SIZE;

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

fn range_is_mapped(
    mgr: &crate::memory_management::UnifiedMemoryManager,
    addr: u64,
    size: u64,
) -> bool {
    let pages = (size / PAGE_SIZE) as usize;
    (0..pages).any(|index| {
        let Some(page) = addr.checked_add(index as u64 * PAGE_SIZE) else {
            return true;
        };
        mgr.page_table_manager()
            .translate_address(page as usize)
            .is_ok()
    })
}

fn range_is_owned_user_memory(
    mgr: &crate::memory_management::UnifiedMemoryManager,
    addr: u64,
    size: u64,
) -> bool {
    let pages = (size / PAGE_SIZE) as usize;
    (0..pages).all(|index| {
        let Some(page) = addr.checked_add(index as u64 * PAGE_SIZE) else {
            return false;
        };
        let Ok(flags) = mgr.page_table_manager().get_page_flags(page as usize) else {
            return false;
        };
        flags.contains(PageTableFlags::USER_ACCESSIBLE)
    })
}

fn overlaps_reserved_user_mapping(addr: u64, size: u64) -> bool {
    ranges_overlap(addr, size, petroleum::vdso::VDSO_USER_BASE, VDSO_SIZE)
}

fn find_free_anon_region(
    rt: &LinuxRuntime,
    mgr: &crate::memory_management::UnifiedMemoryManager,
    size: u64,
    start: u64,
) -> u64 {
    let mut candidate = (start + PAGE_MASK) & !PAGE_MASK;
    loop {
        if candidate.checked_add(size).is_none()
            || candidate.checked_add(size).unwrap() > USER_ADDRESS_LIMIT
        {
            return 0;
        }

        if tracked_range_overlaps(rt, candidate, size) {
            candidate = rt
                .mmap_regions
                .iter()
                .filter(|region| ranges_overlap(region.addr, region.size, candidate, size))
                .filter_map(|region| region.addr.checked_add(region.size))
                .max()
                .unwrap_or(USER_ADDRESS_LIMIT);
            candidate = (candidate + PAGE_MASK) & !PAGE_MASK;
            continue;
        }

        if range_is_mapped(mgr, candidate, size) {
            // Advance past the first mapped page.  The next iteration also
            // checks tracked ranges, so a collision cannot be bypassed by
            // choosing an address supplied by the caller.
            candidate = candidate.saturating_add(PAGE_SIZE);
            continue;
        }
        return candidate;
    }
}

fn track_region(rt: &mut LinuxRuntime, addr: u64, size: u64, prot: i32, flags: i32) {
    rt.mmap_regions.push(LinuxMmapRegion {
        addr,
        size,
        prot,
        flags,
    });
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

pub fn sys_mmap(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let addr_hint = args[0];
    let length = args[1];
    let prot = args[2] as i32;
    let flags = args[3] as i32;
    let offset = args[5];

    let allowed_prot = PROT_READ | PROT_WRITE | PROT_EXEC;
    if (prot & !allowed_prot) != 0 {
        return errno_code(EINVAL);
    }
    if (flags & (MAP_PRIVATE | MAP_SHARED)) == 0
        || (flags & (MAP_PRIVATE | MAP_SHARED)) == (MAP_PRIVATE | MAP_SHARED)
    {
        return errno_code(EINVAL);
    }
    // MAP_FIXED would turn the syscall into an arbitrary page-table overwrite
    // unless every existing mapping is represented by this runtime.  This
    // compatibility layer does not implement that operation safely.
    if (flags & MAP_FIXED) != 0 {
        return errno_code(EINVAL);
    }
    if length == 0 {
        return errno_code(EINVAL);
    }

    let anon = (flags & MAP_ANONYMOUS) != 0;
    if !anon {
        // File-backed mappings are not implemented, and must not accidentally
        // fall through to the anonymous mapping path.
        return errno_code(ENOSYS);
    }
    if offset != 0 {
        return errno_code(EINVAL);
    }

    let (_, aligned_len) = match checked_page_range(0, length, false) {
        Ok(range) => range,
        Err(error) => return errno_code(error),
    };

    let mut memory_guard = crate::memory_management::get_memory_manager().lock();
    let Some(guard) = memory_guard.as_mut() else {
        return errno_code(ENOMEM);
    };

    let hint = if addr_hint == 0 {
        DEFAULT_MMAP_BASE
    } else {
        // A hint is still an address supplied by an untrusted process.  Reject
        // non-canonical/kernel ranges before using or aligning it.
        match checked_page_range(addr_hint, aligned_len, false) {
            Ok((addr, _)) => addr,
            Err(error) => return errno_code(error),
        }
    };

    let addr = find_free_anon_region(rt, guard, aligned_len, hint);
    if addr == 0 {
        return errno_code(ENOMEM);
    }

    let num_pages = (aligned_len / PAGE_SIZE) as usize;
    let frame_alloc = unsafe { petroleum::page_table::constants::get_frame_allocator_mut() };
    let mut mapped_pages = alloc::vec::Vec::with_capacity(num_pages);
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
        let frame = match X86FrameAllocator::<Size4KiB>::allocate_frame(frame_alloc) {
            Some(frame) => frame,
            None => {
                for mapped in mapped_pages {
                    let _ = guard.safe_unmap_page(mapped);
                }
                return errno_code(ENOMEM);
            }
        };

        let mapped = PageTableHelper::map_page(
            guard.page_table_manager_mut(),
            page_vaddr as usize,
            frame.start_address().as_u64() as usize,
            page_flags,
            frame_alloc,
        );
        if mapped.is_err() {
            // `map_page` did not take ownership of the frame on failure.
            frame_alloc.free_frame(frame);
            for mapped in mapped_pages {
                let _ = guard.safe_unmap_page(mapped);
            }
            return errno_code(ENOMEM);
        }
        mapped_pages.push(page_vaddr as usize);
    }

    track_region(rt, addr, aligned_len, prot, flags);
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

    let mut guard = crate::memory_management::get_memory_manager().lock();
    let Some(mgr) = guard.as_mut() else {
        return errno_code(ENOMEM);
    };
    if !range_is_owned_user_memory(mgr, aligned_addr, aligned_len) {
        return errno_code(EINVAL);
    }

    let pages = (aligned_len / PAGE_SIZE) as usize;
    for index in 0..pages {
        let page = aligned_addr + index as u64 * PAGE_SIZE;
        if mgr.safe_unmap_page(page as usize).is_err() {
            return errno_code(EINVAL);
        }
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
    if overlaps_reserved_user_mapping(aligned_addr, aligned_len) {
        return errno_code(EINVAL);
    }

    let mut guard = crate::memory_management::get_memory_manager().lock();
    let Some(mgr) = guard.as_mut() else {
        return errno_code(ENOMEM);
    };
    if !range_is_owned_user_memory(mgr, aligned_addr, aligned_len) {
        return errno_code(EINVAL);
    }

    let pages = (aligned_len / PAGE_SIZE) as usize;
    let ptm = mgr.page_table_manager_mut();
    let mut original_flags = alloc::vec::Vec::with_capacity(pages);
    for index in 0..pages {
        let page = aligned_addr + index as u64 * PAGE_SIZE;
        let flags = match ptm.get_page_flags(page as usize) {
            Ok(flags) => flags,
            Err(_) => return errno_code(EINVAL),
        };
        original_flags.push((page as usize, flags));
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

    for (index, &(page, _)) in original_flags.iter().enumerate() {
        if ptm.set_page_flags(page, page_flags).is_err() {
            for &(rollback_page, rollback_flags) in &original_flags[..index] {
                let _ = ptm.set_page_flags(rollback_page, rollback_flags);
            }
            return errno_code(ENOMEM);
        }
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

    if new_brk == 0 {
        return rt.program_break;
    }

    if new_brk < rt.initial_break
        || new_brk >= USER_ADDRESS_LIMIT
        || new_brk - rt.initial_break > MAX_LINUX_MEMORY
    {
        return rt.program_break;
    }

    let old_brk = rt.program_break;
    let align = PAGE_SIZE;

    if new_brk > old_brk {
        let start_page = (old_brk + align - 1) & !(align - 1);
        let end_page = (new_brk + align - 1) & !(align - 1);

        if end_page > start_page {
            let num_pages = ((end_page - start_page) / align) as usize;
            let mut memory_guard = crate::memory_management::get_memory_manager().lock();
            let Some(mgr) = memory_guard.as_mut() else {
                return old_brk;
            };
            let growth = end_page - start_page;
            if tracked_range_overlaps(rt, start_page, growth)
                || range_is_mapped(mgr, start_page, growth)
            {
                return old_brk;
            }

            let frame_alloc =
                unsafe { petroleum::page_table::constants::get_frame_allocator_mut() };

            for i in 0..num_pages {
                let page_vaddr = start_page + (i as u64) * align;
                let frame = match X86FrameAllocator::<Size4KiB>::allocate_frame(frame_alloc) {
                    Some(f) => f,
                    None => {
                        // Rollback previously mapped pages on OOM
                        for j in 0..i {
                            let unmap_vaddr = (start_page + (j as u64) * align) as usize;
                            if mgr
                                .page_table_manager()
                                .translate_address(unmap_vaddr)
                                .is_ok()
                            {
                                let _ = mgr.safe_unmap_page(unmap_vaddr);
                            }
                        }
                        return old_brk;
                    }
                };
                let page_flags = PageTableFlags::PRESENT
                    | PageTableFlags::WRITABLE
                    | PageTableFlags::USER_ACCESSIBLE;

                // Create ptm in a narrow scope to avoid aliasing with mgr accesses below
                let map_result = {
                    let ptm = mgr.page_table_manager_mut() as *mut _;
                    let ptm = unsafe { &mut *ptm };
                    PageTableHelper::map_page(
                        ptm,
                        page_vaddr as usize,
                        frame.start_address().as_u64() as usize,
                        page_flags,
                        frame_alloc,
                    )
                };

                if map_result.is_err() {
                    frame_alloc.free_frame(frame);
                    for j in 0..i {
                        let unmap_vaddr = (start_page + (j as u64) * align) as usize;
                        let _ = mgr.safe_unmap_page(unmap_vaddr);
                    }
                    return old_brk;
                }
            }
        }
    }
    // Note: shrinking (new_brk < old_brk) is intentionally skipped for now

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
}
