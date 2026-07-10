use core::alloc::Layout;

use x86_64::VirtAddr;

use super::interface::{SyscallError, SyscallResult};
use super::types::{Handle, HandlePerms, KernelObject};
use crate::process;

pub(crate) fn with_current_fd_table<F, R>(f: F) -> Result<R, SyscallError>
where
    F: FnOnce(&mut process::FdTable) -> Result<R, SyscallError>,
{
    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    match process::SCHEDULER.with_process(pid, |p| {
        let mut ft = p.resources.fd_table.lock();
        f(&mut *ft)
    }) {
        Some(r) => r,
        None => Err(SyscallError::NoSuchProcess),
    }
}

pub(crate) fn with_current_handle_table<F, R>(f: F) -> Result<R, SyscallError>
where
    F: FnOnce(&mut crate::process::HandleTable) -> Result<R, SyscallError>,
{
    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    match process::SCHEDULER.with_process(pid, |p| {
        let mut ht = p.resources.handle_table.lock();
        f(&mut *ht)
    }) {
        Some(r) => r,
        None => Err(SyscallError::NoSuchProcess),
    }
}

pub(crate) fn with_kernel_mut_result<F>(f: F) -> SyscallResult
where
    F: FnOnce(&mut crate::contexts::KernelContext) -> SyscallResult,
{
    crate::contexts::kernel::with_kernel_mut(f).ok_or(SyscallError::NotSupported)?
}

pub(crate) fn alloc_handle(obj: KernelObject) -> Result<u64, SyscallError> {
    with_current_handle_table(|ht| {
        let h = ht.alloc(obj);
        Ok(h.raw())
    })
}

pub(crate) fn with_handle_mut<F, R>(h: Handle, f: F) -> Result<R, SyscallError>
where
    F: FnOnce(&mut KernelObject) -> Result<R, SyscallError>,
{
    with_current_handle_table(|ht| match ht.get_mut(h) {
        Some(obj) => f(obj),
        None => Err(SyscallError::BadHandle),
    })
}

pub(crate) fn with_handle<F, R>(h: Handle, f: F) -> Result<R, SyscallError>
where
    F: FnOnce(&KernelObject) -> Result<R, SyscallError>,
{
    with_current_handle_table(|ht| match ht.get(h) {
        Some(obj) => f(obj),
        None => Err(SyscallError::BadHandle),
    })
}

pub(crate) fn check_handle_permission(h: Handle, required: HandlePerms) -> Result<(), SyscallError> {
    with_current_handle_table(|ht| {
        if !ht.check_perm(h, required) {
            Err(SyscallError::PermissionDenied)
        } else {
            Ok(())
        }
    })
}

pub(crate) fn alloc_kernel_stack() -> Result<(*mut u8, VirtAddr), SyscallError> {
    let layout = Layout::from_size_align(crate::heap::KERNEL_STACK_SIZE, 16).unwrap();
    let ptr = petroleum::common::memory::allocate_layout(layout)
        .map_err(|_| SyscallError::OutOfMemory)?;
    let top = VirtAddr::new(ptr as u64 + crate::heap::KERNEL_STACK_SIZE as u64);
    Ok((ptr, top))
}

pub(crate) fn free_kernel_stack(ptr: *mut u8) {
    let layout = Layout::from_size_align(crate::heap::KERNEL_STACK_SIZE, 16).unwrap();
    unsafe { petroleum::common::memory::deallocate_layout(ptr, layout) };
}
