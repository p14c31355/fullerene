use alloc::vec::Vec;

use core::sync::atomic::{AtomicU64, Ordering};
use petroleum::common::memory::UserSlice;
use petroleum::page_table::types::PageTableHelper;
use x86_64::VirtAddr;

use super::interface::{SyscallError, SyscallResult};
use super::process::with_kernel_mut_result;

const PROT_READ: u64 = 1;
const PROT_WRITE: u64 = 2;
const PROT_EXEC: u64 = 4;

fn rollback_mapped_pages(memory: &mut crate::contexts::memory::MemoryContext, pages: &[usize]) {
    if let Some(mgr) = memory.manager.as_mut() {
        for vaddr in pages {
            let _ = mgr.safe_unmap_page(*vaddr);
        }
    }
}

pub(crate) fn syscall_map_memory(addr_hint: u64, length: u64, flags: u64) -> SyscallResult {
    let len = length as usize;
    if len == 0 || len > (128 << 20) {
        return Err(SyscallError::InvalidArgument);
    }

    if addr_hint != 0 {
        let end_vaddr = addr_hint
            .checked_add(length)
            .ok_or(SyscallError::InvalidArgument)?;
        let start_addr = VirtAddr::try_new(addr_hint).map_err(|_| SyscallError::InvalidArgument)?;
        let end_addr =
            VirtAddr::try_new(end_vaddr - 1).map_err(|_| SyscallError::InvalidArgument)?;
        if !petroleum::is_user_address(start_addr) || !petroleum::is_user_address(end_addr) {
            return Err(SyscallError::PermissionDenied);
        }
    }

    let prot = (flags >> 16) & 0xFF;

    let mut pt_flags = x86_64::structures::paging::PageTableFlags::empty();
    if (prot & PROT_READ) != 0 {
        pt_flags |= x86_64::structures::paging::PageTableFlags::PRESENT;
    }
    if (prot & PROT_WRITE) != 0 {
        pt_flags |= x86_64::structures::paging::PageTableFlags::WRITABLE;
    }
    if (prot & PROT_EXEC) == 0 {
        pt_flags |= x86_64::structures::paging::PageTableFlags::NO_EXECUTE;
    }
    pt_flags |= x86_64::structures::paging::PageTableFlags::USER_ACCESSIBLE;

    with_kernel_mut_result(|k| -> SyscallResult {
        let memory = &mut k.memory;

        let virt_base = if addr_hint != 0
            && addr_hint % 4096 == 0
            && petroleum::is_user_address(VirtAddr::new(addr_hint))
        {
            addr_hint as usize
        } else {
            static NEXT_VADDR: AtomicU64 = AtomicU64::new(0x100_0000_0000);
            let aligned_len = (len + 4095) & !4095;
            NEXT_VADDR.fetch_add(aligned_len as u64, Ordering::Relaxed) as usize
        };

        let num_pages = (len + 4095) / 4096;
        let mut mapped_pages: Vec<usize> = Vec::with_capacity(num_pages);
        for i in 0..num_pages {
            let frame = memory.allocate_frame().map_err(|_| {
                rollback_mapped_pages(memory, &mapped_pages);
                SyscallError::OutOfMemory
            })?;
            let vaddr = virt_base + i * 4096;
            memory.map_page(vaddr, frame, pt_flags).map_err(|_| {
                let _ = memory.free_frame(frame);
                rollback_mapped_pages(memory, &mapped_pages);
                SyscallError::OutOfMemory
            })?;
            mapped_pages.push(vaddr);
        }

        Ok(virt_base as u64)
    })
}

pub(crate) fn syscall_unmap_memory(addr: u64, length: u64) -> SyscallResult {
    let len = length as usize;
    if len == 0 || (addr % 4096) != 0 {
        return Err(SyscallError::InvalidArgument);
    }
    let end_vaddr = addr
        .checked_add(length)
        .ok_or(SyscallError::InvalidArgument)?;
    let start_addr = VirtAddr::try_new(addr).map_err(|_| SyscallError::InvalidArgument)?;
    let end_addr = VirtAddr::try_new(end_vaddr - 1).map_err(|_| SyscallError::InvalidArgument)?;
    if !petroleum::is_user_address(start_addr) || !petroleum::is_user_address(end_addr) {
        return Err(SyscallError::PermissionDenied);
    }

    with_kernel_mut_result(|k| -> SyscallResult {
        let memory = &mut k.memory;
        let num_pages = (len + 4095) / 4096;
        let mgr = memory.manager.as_mut().ok_or(SyscallError::OutOfMemory)?;
        for i in 0..num_pages {
            let vaddr = addr as usize + i * 4096;
            mgr.safe_unmap_page(vaddr)
                .map_err(|_| SyscallError::OutOfMemory)?;
        }
        Ok(0)
    })
}

pub(crate) fn syscall_protect_memory(addr: u64, length: u64, prot: u64) -> SyscallResult {
    let len = length as usize;
    if len == 0 || len > (128 << 20) || (addr % 4096) != 0 {
        return Err(SyscallError::InvalidArgument);
    }
    let start_addr = VirtAddr::try_new(addr).map_err(|_| SyscallError::InvalidArgument)?;
    let end_vaddr = addr
        .checked_add(length - 1)
        .ok_or(SyscallError::InvalidArgument)?;
    let end_addr = VirtAddr::try_new(end_vaddr).map_err(|_| SyscallError::InvalidArgument)?;
    if !petroleum::is_user_address(start_addr) || !petroleum::is_user_address(end_addr) {
        return Err(SyscallError::PermissionDenied);
    }

    let read = (prot & PROT_READ) != 0;
    let write = (prot & PROT_WRITE) != 0;
    let exec = (prot & PROT_EXEC) != 0;

    with_kernel_mut_result(|k| -> SyscallResult {
        let memory = &mut k.memory;
        let mgr = memory.manager.as_mut().ok_or(SyscallError::OutOfMemory)?;
        let ptm = mgr.page_table_manager_mut();
        let num_pages = (len + 4095) / 4096;

        // First pass: validate all pages are mapped and collect original flags
        let mut page_info: Vec<(usize, x86_64::structures::paging::PageTableFlags)> =
            Vec::with_capacity(num_pages);
        for i in 0..num_pages {
            let vaddr = addr as usize + i * 4096;

            // Validate page is mapped (translate_address returns error if not mapped)
            ptm.translate_address(vaddr)
                .map_err(|_| SyscallError::InvalidArgument)?;

            // Get current flags to save for potential rollback
            let original_flags = ptm
                .get_page_flags(vaddr)
                .map_err(|_| SyscallError::InvalidArgument)?;

            page_info.push((vaddr, original_flags));
        }

        // Second pass: apply new flags to all pages
        for (idx, &(vaddr, _)) in page_info.iter().enumerate() {
            // Build new flags based on protection arguments
            let mut flags = x86_64::structures::paging::PageTableFlags::empty();

            // Handle PROT_NONE: when no permissions are requested, clear PRESENT to make inaccessible
            if prot == 0 {
                // PROT_NONE: clear PRESENT but keep USER_ACCESSIBLE to maintain it's a user page
                flags = x86_64::structures::paging::PageTableFlags::USER_ACCESSIBLE;
            } else {
                // At least one permission is requested
                if read {
                    flags |= x86_64::structures::paging::PageTableFlags::PRESENT;
                }
                // Note: on x86_64, there's no separate read permission bit; PRESENT implies readable
                // So if write or exec is set without read, we still need PRESENT
                if write || exec {
                    flags |= x86_64::structures::paging::PageTableFlags::PRESENT;
                }

                flags |= x86_64::structures::paging::PageTableFlags::USER_ACCESSIBLE;

                if write {
                    flags |= x86_64::structures::paging::PageTableFlags::WRITABLE;
                }
                if !exec {
                    flags |= x86_64::structures::paging::PageTableFlags::NO_EXECUTE;
                }
            }

            // Try to update flags; rollback on failure
            if ptm.set_page_flags(vaddr, flags).is_err() {
                // Rollback: restore all previously changed pages
                for &(rollback_vaddr, rollback_flags) in &page_info[..idx] {
                    let _ = ptm.set_page_flags(rollback_vaddr, rollback_flags);
                }
                return Err(SyscallError::OutOfMemory);
            }
        }

        Ok(0)
    })
}

pub(crate) fn syscall_query_memory(info_buf: *mut u8, buf_size: usize) -> SyscallResult {
    if info_buf.is_null() || buf_size < fullerene_abi::MemoryInfo::BYTE_SIZE {
        return Err(SyscallError::InvalidArgument);
    }
    petroleum::validate_user_buffer(
        info_buf as usize,
        fullerene_abi::MemoryInfo::BYTE_SIZE,
        false,
    )?;

    let info = fullerene_abi::MemoryInfo {
        page_size: 4096,
        ..fullerene_abi::MemoryInfo::default()
    };
    let bytes = info.to_ne_bytes();
    let slice =
        UserSlice::new(info_buf, bytes.len(), true).map_err(|_| SyscallError::AddressFault)?;
    unsafe { slice.copy_to_user(&bytes) }.map_err(|_| SyscallError::AddressFault)?;
    Ok(bytes.len() as u64)
}
