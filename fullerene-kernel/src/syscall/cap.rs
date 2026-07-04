use alloc::sync::Arc;

use super::interface::{SyscallError, SyscallResult};
use super::process::{alloc_handle, check_handle_permission, with_current_handle_table};
use super::types::*;
use crate::process;

pub(crate) fn syscall_handle_transfer(target_pid: u64, handle: u64) -> SyscallResult {
    let h = Handle::from_raw(handle);
    check_handle_permission(h, HandlePerms::TRANSFER)?;
    let target = process::ProcessId(target_pid);

    let mut obj = Some(with_current_handle_table(|ht| {
        ht.remove(h).ok_or(SyscallError::BadHandle)
    })?);

    let new_handle = process::PROCESS_MANAGER.with_process(target, |p| {
        let mut ht = p.resources.handle_table.lock();
        let owned = obj.take().unwrap();
        ht.alloc(owned)
    });

    match new_handle {
        Some(new_h) => Ok(new_h.raw()),
        None => {
            let owned = obj.take().unwrap();
            let _ = with_current_handle_table(|ht| {
                ht.alloc(owned);
                Ok::<(), SyscallError>(())
            });
            Err(SyscallError::NoSuchProcess)
        }
    }
}

pub(crate) fn syscall_handle_duplicate(handle: u64) -> SyscallResult {
    let h = Handle::from_raw(handle);
    check_handle_permission(h, HandlePerms::DUPLICATE)?;

    let new_obj = with_current_handle_table(|ht| {
        let obj = ht.get(h).ok_or(SyscallError::BadHandle)?;
        let new_obj = match obj {
            KernelObject::Event(e) => KernelObject::Event(EventState {
                inner: Arc::clone(&e.inner),
            }),
            KernelObject::Thread(t) => KernelObject::Thread(ThreadState {
                inner: Arc::clone(&t.inner),
            }),
            KernelObject::Channel(ch) => KernelObject::Channel(ChannelState {
                inner: Arc::clone(&ch.inner),
            }),
            KernelObject::Window(w) => KernelObject::Window(WindowState {
                window_id: w.window_id,
                pid: w.pid,
            }),
            KernelObject::Pipe(p) => KernelObject::Pipe(PipeState {
                buffer: Arc::clone(&p.buffer),
                is_read_end: p.is_read_end,
            }),
            _ => return Err(SyscallError::NotSupported),
        };
        Ok(new_obj)
    })?;

    alloc_handle(new_obj)
}

pub(crate) fn syscall_handle_revoke(handle: u64) -> SyscallResult {
    let h = Handle::from_raw(handle);
    with_current_handle_table(|ht| {
        ht.remove(h).ok_or(SyscallError::BadHandle)?;
        Ok(0)
    })
}
