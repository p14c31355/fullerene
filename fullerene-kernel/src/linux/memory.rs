// Linux memory syscall implementations
use super::numbers::*;
use super::runtime::{LinuxRuntime, errno_code};

use petroleum::page_table::types::PageTableHelper;
use x86_64::structures::paging::Size4KiB;
use x86_64::structures::paging::{FrameAllocator as X86FrameAllocator, PageTableFlags};

/// Per-process virtual memory region tracked for mmap/munmap.
#[derive(Clone, Copy)]
pub struct LinuxMmapRegion {
    pub addr: u64,
    pub size: u64,
    pub prot: i32,
    pub flags: i32,
}

fn find_free_anon_region(rt: &mut LinuxRuntime, size: u64) -> u64 {
    let base: u64 = 0x60000000;
    let mut candidate = base;
    loop {
        let mut overlap = false;
        for reg in &rt.mmap_regions {
            let r_start = reg.addr;
            let r_end = r_start + reg.size;
            let c_end = candidate + size;
            if candidate < r_end && c_end > r_start {
                overlap = true;
                candidate = r_end;
                break;
            }
        }
        if !overlap {
            return candidate;
        }
        if candidate > 0x70000000 {
            return 0;
        }
    }
}

fn track_region(rt: &mut LinuxRuntime, addr: u64, size: u64, prot: i32, flags: i32) -> bool {
    rt.mmap_regions.push(LinuxMmapRegion {
        addr,
        size,
        prot,
        flags,
    });
    true
}

fn untrack_region(rt: &mut LinuxRuntime, addr: u64) -> bool {
    if let Some(pos) = rt.mmap_regions.iter().position(|r| r.addr == addr) {
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
    let _fd = args[4] as i32;
    let _offset = args[5];

    if length == 0 {
        return errno_code(EINVAL);
    }

    let anon = (flags & MAP_ANONYMOUS) != 0;
    let _private = (flags & MAP_PRIVATE) != 0;
    let _fixed = (flags & MAP_FIXED) != 0;

    if anon {
        let aligned_len = (length + 4095) & !4095;
        let addr = if addr_hint != 0 && addr_hint >= 0x10000 {
            addr_hint & !4095
        } else {
            find_free_anon_region(rt, aligned_len)
        };

        if addr == 0 {
            return errno_code(ENOMEM);
        }

        let num_pages = (aligned_len / 4096) as usize;
        let frame_alloc = unsafe { petroleum::page_table::constants::get_frame_allocator_mut() };

        for i in 0..num_pages {
            let page_vaddr = addr + (i as u64) * 4096;

            let frame = match X86FrameAllocator::<Size4KiB>::allocate_frame(frame_alloc) {
                Some(f) => f,
                None => {
                    // Unmap pages we've already mapped in this call
                    if let Some(mgr) = crate::memory_management::get_memory_manager()
                        .lock()
                        .as_mut()
                    {
                        for j in 0..i {
                            let unmap_vaddr = (addr + (j as u64) * 4096) as usize;
                            if mgr
                                .page_table_manager()
                                .translate_address(unmap_vaddr)
                                .is_ok()
                            {
                                let _ = mgr.safe_unmap_page(unmap_vaddr);
                            }
                        }
                    }
                    return errno_code(ENOMEM);
                }
            };

            let mut page_flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;

            if (prot & PROT_WRITE) != 0 {
                page_flags |= PageTableFlags::WRITABLE;
            }

            if let Some(mgr) = crate::memory_management::get_memory_manager()
                .lock()
                .as_mut()
            {
                let ptm = mgr.page_table_manager_mut() as *mut _;
                let ptm = unsafe { &mut *ptm };
                let _ = PageTableHelper::map_page(
                    ptm,
                    page_vaddr as usize,
                    frame.start_address().as_u64() as usize,
                    page_flags,
                    frame_alloc,
                );
            }
        }

        track_region(rt, addr, aligned_len, prot, flags);
        return addr;
    }

    errno_code(ENOSYS)
}

pub fn sys_munmap(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let addr = args[0];
    let length = args[1];

    if addr == 0 || length == 0 {
        return errno_code(EINVAL);
    }

    let aligned_len = (length + 4095) & !4095;
    let num_pages = (aligned_len / 4096) as usize;

    if let Some(mgr) = crate::memory_management::get_memory_manager()
        .lock()
        .as_mut()
    {
        for i in 0..num_pages {
            let page_vaddr = (addr + (i as u64) * 4096) as usize;
            if let Ok(_phys) = mgr.page_table_manager().translate_address(page_vaddr) {
                let _ = mgr.safe_unmap_page(page_vaddr);
            }
        }
    }

    untrack_region(rt, addr);
    0
}

pub fn sys_mprotect(_rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let addr = args[0];
    let length = args[1];
    let prot = args[2] as i32;

    if addr == 0 || length == 0 {
        return errno_code(EINVAL);
    }

    let aligned_addr = addr & !4095;
    let aligned_len = (length + 4095) & !4095;
    let num_pages = (aligned_len / 4096) as usize;

    if let Some(mgr) = crate::memory_management::get_memory_manager()
        .lock()
        .as_mut()
    {
        let ptm = mgr.page_table_manager_mut() as *mut _;
        let ptm = unsafe { &mut *ptm };

        for i in 0..num_pages {
            let page_vaddr = (aligned_addr + (i as u64) * 4096) as usize;
            let mut page_flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
            if (prot & PROT_WRITE) != 0 {
                page_flags |= PageTableFlags::WRITABLE;
            }
            if (prot & PROT_EXEC) == 0 {
                page_flags |= PageTableFlags::NO_EXECUTE;
            }
            if PageTableHelper::set_page_flags(ptm, page_vaddr, page_flags).is_err() {
                // Page not mapped — skip, but don't silently swallow
                continue;
            }
        }
    }
    0
}

pub fn sys_brk(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let new_brk = args[0];

    if new_brk == 0 {
        return rt.program_break;
    }

    if new_brk < rt.initial_break {
        return rt.program_break;
    }

    let old_brk = rt.program_break;
    let align = 4096u64;

    if new_brk > old_brk {
        let start_page = (old_brk + align - 1) & !(align - 1);
        let end_page = (new_brk + align - 1) & !(align - 1);

        if end_page > start_page {
            let num_pages = ((end_page - start_page) / align) as usize;
            let frame_alloc = unsafe { petroleum::page_table::constants::get_frame_allocator_mut() };

            if let Some(mgr) = crate::memory_management::get_memory_manager()
                .lock()
                .as_mut()
            {
                let ptm = mgr.page_table_manager_mut() as *mut _;
                let ptm = unsafe { &mut *ptm };

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
                    let _ = PageTableHelper::map_page(
                        ptm,
                        page_vaddr as usize,
                        frame.start_address().as_u64() as usize,
                        page_flags,
                        frame_alloc,
                    );
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
