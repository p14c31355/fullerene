use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;

use crate::map_handle;
use petroleum::common::memory::UserSlice;

use super::interface::{SyscallError, SyscallResult};
use super::process::{alloc_handle, check_handle_permission, with_handle_mut};
use super::types::*;
use crate::process;

pub(crate) fn syscall_channel_create(_flags: u64) -> SyscallResult {
    let inner = Arc::new(Mutex::new(ChannelInner {
        messages: Vec::with_capacity(16),
        waiters: Vec::new(),
        max_messages: 64,
    }));
    alloc_handle(KernelObject::Channel(ChannelState { inner }))
}

pub(crate) fn syscall_channel_send(
    handle: u64,
    data_ptr: *const u8,
    data_size: u64,
) -> SyscallResult {
    let h = Handle::from_raw(handle);
    check_handle_permission(h, HandlePerms::WRITE)?;
    let size = data_size as usize;
    if size == 0 || size > 65536 {
        return Err(SyscallError::InvalidArgument);
    }

    let slice = UserSlice::new(data_ptr as *mut u8, size, false)
        .map_err(|_| SyscallError::InvalidArgument)?;

    let mut msg_vec = vec![0u8; size];
    unsafe { slice.copy_from_user(&mut msg_vec) }.map_err(|_| SyscallError::InvalidArgument)?;

    let recv_waiters: Vec<process::ProcessId> = with_handle_mut(h, |obj| {
        let channel = map_handle!(obj, Channel, ch);
        let mut inner = channel.inner.lock();
        if inner.messages.len() >= inner.max_messages {
            return Err(SyscallError::Again);
        }
        inner.messages.push(msg_vec);
        Ok(core::mem::take(&mut inner.waiters))
    })?;

    for pid in recv_waiters {
        crate::process::unblock_process(pid);
    }

    Ok(size as u64)
}

pub(crate) fn syscall_channel_recv(handle: u64, buf: *mut u8, buf_size: u64) -> SyscallResult {
    let h = Handle::from_raw(handle);
    check_handle_permission(h, HandlePerms::READ)?;
    let max = buf_size as usize;
    if buf.is_null() || max == 0 || max > 65536 {
        return Err(SyscallError::InvalidArgument);
    }
    petroleum::validate_user_buffer(buf as usize, max, false)?;
    let slice = UserSlice::new(buf, max, true).map_err(|_| SyscallError::InvalidArgument)?;

    let msg: Option<Vec<u8>> = with_handle_mut(h, |obj| {
        let channel = map_handle!(obj, Channel, ch);
        let mut inner = channel.inner.lock();
        if !inner.messages.is_empty() {
            Ok(Some(inner.messages.remove(0)))
        } else {
            Ok(None)
        }
    })?;

    if let Some(msg) = msg {
        let copy_len = msg.len().min(max);
        let mut kernel_buf = vec![0u8; max];
        kernel_buf[..copy_len].copy_from_slice(&msg[..copy_len]);
        unsafe { slice.copy_to_user(&kernel_buf) }.map_err(|_| SyscallError::InvalidArgument)?;
        Ok(copy_len as u64)
    } else {
        Err(SyscallError::WouldBlock)
    }
}

pub(crate) fn syscall_pipe_create(buf: *mut u64) -> SyscallResult {
    if buf.is_null() {
        return Err(SyscallError::InvalidArgument);
    }
    petroleum::validate_user_buffer(buf as usize, 16, false)?;

    let shared_buffer = Arc::new(Mutex::new(Vec::with_capacity(4096)));

    let read_end = PipeState {
        buffer: Arc::clone(&shared_buffer),
        is_read_end: true,
    };
    let write_end = PipeState {
        buffer: shared_buffer,
        is_read_end: false,
    };
    let read_h = alloc_handle(KernelObject::Pipe(read_end))?;
    let write_h = match alloc_handle(KernelObject::Pipe(write_end)) {
        Ok(h) => h,
        Err(e) => {
            let _ = super::cap::syscall_handle_revoke(read_h);
            return Err(e);
        }
    };

    let slice =
        UserSlice::new(buf as *mut u8, 16, true).map_err(|_| SyscallError::InvalidArgument)?;
    let mut kernel_buf = [0u8; 16];
    kernel_buf[0..8].copy_from_slice(&read_h.to_ne_bytes());
    kernel_buf[8..16].copy_from_slice(&write_h.to_ne_bytes());
    if unsafe { slice.copy_to_user(&kernel_buf) }.is_err() {
        let _ = super::cap::syscall_handle_revoke(read_h);
        let _ = super::cap::syscall_handle_revoke(write_h);
        return Err(SyscallError::InvalidArgument);
    }

    Ok(0)
}
