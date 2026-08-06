//! Capability-backed shared user pages.
//!
//! This is the first zero-copy IPC layer.  It grants ordinary RAM pages to a
//! process and lets an explicitly transferred/duplicated handle map those
//! same pages into another process.  Device DMA ownership and cache
//! transitions are intentionally not implied by this API.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use petroleum::common::memory::physical_to_virtual;
use petroleum::page_table::PageTableHelper;
use petroleum::page_table::allocator::traits::FrameAllocator;
use spin::Mutex;
use x86_64::{VirtAddr, structures::paging::PageTableFlags};

use fullerene_abi::shared_buffer_flags;

use super::interface::{SyscallError, SyscallResult};
use super::process::{alloc_handle, check_handle_permission, with_handle};
use super::types::{Handle, HandlePerms, KernelObject, SharedBufferInner, SharedBufferMapping};
use crate::process::{self, ProcessId};

const PAGE_SIZE: usize = 4096;
const MAX_SHARED_BUFFER_SIZE: usize = 128 << 20;
const SHARED_BUFFER_ADDRESS_BASE: u64 = 0x2000_0000_0000;

static NEXT_SHARED_BUFFER_ADDRESS: AtomicU64 = AtomicU64::new(SHARED_BUFFER_ADDRESS_BASE);

fn validate_user_range(address: usize, length: usize) -> Result<(), SyscallError> {
    let end = address
        .checked_add(length.checked_sub(1).ok_or(SyscallError::InvalidArgument)?)
        .ok_or(SyscallError::InvalidArgument)?;
    let start = VirtAddr::try_new(address as u64).map_err(|_| SyscallError::InvalidArgument)?;
    let end = VirtAddr::try_new(end as u64).map_err(|_| SyscallError::InvalidArgument)?;
    if !petroleum::is_user_address(start) || !petroleum::is_user_address(end) {
        return Err(SyscallError::PermissionDenied);
    }
    Ok(())
}

fn map_flags(rights: u64) -> PageTableFlags {
    let mut flags =
        PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE | PageTableFlags::NO_EXECUTE;
    if rights & shared_buffer_flags::WRITE != 0 {
        flags |= PageTableFlags::WRITABLE;
    }
    flags
}

fn buffer_from_handle(handle: Handle) -> Result<Arc<Mutex<SharedBufferInner>>, SyscallError> {
    with_handle(handle, |object| match object {
        KernelObject::SharedBuffer(buffer) => Ok(Arc::clone(&buffer.inner)),
        _ => Err(SyscallError::BadHandle),
    })
}

fn free_frames(frames: &mut alloc::vec::Vec<usize>) {
    petroleum::page_table::constants::with_frame_allocator(|allocator| {
        use petroleum::page_table::allocator::traits::FrameAllocatorExt;
        for frame in frames.drain(..) {
            allocator.deallocate_frame(petroleum::page_table::types::PhysFrame {
                start_address: frame as u64,
            });
        }
    });
}

/// Allocate a kernel-owned page-backed buffer and return its capability.
pub(crate) fn syscall_shared_buffer_create(length: u64, flags: u64) -> SyscallResult {
    let requested = usize::try_from(length).map_err(|_| SyscallError::InvalidArgument)?;
    if requested == 0 || requested > MAX_SHARED_BUFFER_SIZE {
        return Err(SyscallError::InvalidArgument);
    }
    let allowed =
        shared_buffer_flags::READ | shared_buffer_flags::WRITE | shared_buffer_flags::ZEROED;
    if flags & !allowed != 0
        || flags & (shared_buffer_flags::READ | shared_buffer_flags::WRITE) == 0
    {
        return Err(SyscallError::InvalidArgument);
    }

    let length = requested
        .checked_add(PAGE_SIZE - 1)
        .ok_or(SyscallError::InvalidArgument)?
        & !(PAGE_SIZE - 1);
    let page_count = length / PAGE_SIZE;
    let mut frames = alloc::vec::Vec::with_capacity(page_count);

    let allocation = petroleum::page_table::constants::with_frame_allocator(|allocator| {
        for _ in 0..page_count {
            let frame = match allocator.allocate() {
                Ok(frame) => frame,
                Err(_) => return false,
            };
            frames.push(frame.start_address() as usize);
        }
        true
    });
    if !allocation {
        free_frames(&mut frames);
        return Err(SyscallError::OutOfMemory);
    }

    // Always clear newly granted pages.  ZEROED is retained as an ABI flag so
    // callers can document the expected initialization contract.
    for &frame in &frames {
        unsafe {
            core::ptr::write_bytes(physical_to_virtual(frame) as *mut u8, 0, PAGE_SIZE);
        }
    }

    let object = KernelObject::SharedBuffer(super::types::SharedBufferState {
        inner: Arc::new(Mutex::new(SharedBufferInner {
            frames,
            length,
            flags,
            mappings: alloc::vec::Vec::new(),
        })),
    });
    match alloc_handle(object) {
        Ok(handle) => Ok(handle),
        Err(error) => Err(error),
    }
}

/// Map a shared buffer into the current process.
pub(crate) fn syscall_shared_buffer_map(
    raw_handle: u64,
    addr_hint: u64,
    requested_flags: u64,
) -> SyscallResult {
    let handle = Handle::from_raw(raw_handle);
    check_handle_permission(handle, HandlePerms::READ)?;
    let inner = buffer_from_handle(handle)?;
    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;

    let (length, rights, frames) = {
        let buffer = inner.lock();
        let rights = if requested_flags == 0 {
            buffer.flags & (shared_buffer_flags::READ | shared_buffer_flags::WRITE)
        } else {
            requested_flags
        };
        if rights & !(shared_buffer_flags::READ | shared_buffer_flags::WRITE) != 0
            || rights & (shared_buffer_flags::READ | shared_buffer_flags::WRITE) == 0
            || rights & !buffer.flags != 0
        {
            return Err(SyscallError::PermissionDenied);
        }
        (buffer.length, rights, buffer.frames.clone())
    };

    let address = if addr_hint == 0 {
        let increment = length as u64;
        NEXT_SHARED_BUFFER_ADDRESS
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(increment)
            })
            .map_err(|_| SyscallError::OutOfMemory)?
    } else {
        if addr_hint as usize % PAGE_SIZE != 0 {
            return Err(SyscallError::InvalidArgument);
        }
        addr_hint
    };
    validate_user_range(address as usize, length)?;

    let map_result = process::SCHEDULER
        .with_process(ProcessId(pid.0), |process| {
            let page_table = process
                .page_table
                .as_mut()
                .ok_or(SyscallError::NoSuchProcess)?;
            let result = petroleum::page_table::constants::with_frame_allocator(|allocator| {
                let flags = map_flags(rights);
                let mut mapped = 0usize;
                for (index, frame) in frames.iter().enumerate() {
                    let virtual_address = address as usize + index * PAGE_SIZE;
                    if page_table.translate_address(virtual_address).is_ok() {
                        for rollback in 0..mapped {
                            let _ = page_table.unmap_page(address as usize + rollback * PAGE_SIZE);
                        }
                        return Err(SyscallError::InvalidArgument);
                    }
                    if page_table
                        .map_page(virtual_address, *frame, flags, allocator)
                        .is_err()
                    {
                        for rollback in 0..mapped {
                            let _ = page_table.unmap_page(address as usize + rollback * PAGE_SIZE);
                        }
                        return Err(SyscallError::OutOfMemory);
                    }
                    mapped += 1;
                }
                Ok(())
            });
            result
        })
        .ok_or(SyscallError::NoSuchProcess)?;
    map_result?;

    inner.lock().mappings.push(SharedBufferMapping {
        pid,
        address: address as usize,
        length,
    });
    Ok(address)
}

/// Remove one mapping from the current process while retaining the capability.
pub(crate) fn syscall_shared_buffer_unmap(raw_handle: u64, address: u64) -> SyscallResult {
    let handle = Handle::from_raw(raw_handle);
    let inner = buffer_from_handle(handle)?;
    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    let mapping = {
        let buffer = inner.lock();
        buffer
            .mappings
            .iter()
            .find(|mapping| mapping.pid == pid && mapping.address == address as usize)
            .map(|mapping| (mapping.address, mapping.length))
            .ok_or(SyscallError::InvalidArgument)?
    };

    let unmap_result: Result<(), SyscallError> = process::SCHEDULER
        .with_process(pid, |process| {
            let page_table = process
                .page_table
                .as_mut()
                .ok_or(SyscallError::NoSuchProcess)?;
            for index in 0..(mapping.1 / PAGE_SIZE) {
                page_table
                    .unmap_page(mapping.0 + index * PAGE_SIZE)
                    .map_err(|_| SyscallError::InvalidArgument)?;
            }
            Ok(())
        })
        .ok_or(SyscallError::NoSuchProcess)?;
    unmap_result?;

    inner.lock().mappings.retain(|entry| {
        !(entry.pid == pid && entry.address == mapping.0 && entry.length == mapping.1)
    });
    Ok(0)
}

/// Return whether a shared-buffer capability still has mappings in a process.
pub(crate) fn has_mappings_for_current_process(raw_handle: u64) -> Result<bool, SyscallError> {
    let handle = Handle::from_raw(raw_handle);
    let inner = buffer_from_handle(handle)?;
    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    Ok(inner
        .lock()
        .mappings
        .iter()
        .any(|mapping| mapping.pid == pid))
}
