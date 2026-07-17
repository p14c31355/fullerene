//! Native filesystem and terminal I/O syscalls.

use alloc::vec;
use core::ffi::c_int;

use petroleum::common::memory::UserSlice;

use super::interface::{SyscallError, SyscallResult, copy_user_string};
use super::process::with_current_fd_table;
use crate::linux::{O_APPEND, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY};

const MAX_IO_BYTES: usize = 65_536;
const MAX_PATH_BYTES: usize = 256;

pub(crate) fn syscall_read(fd: c_int, buffer: *mut u8, count: usize) -> SyscallResult {
    let count = count.min(MAX_IO_BYTES);
    if count == 0 {
        return Ok(0);
    }

    let slice = UserSlice::new(buffer, count, true).map_err(|_| SyscallError::InvalidArgument)?;
    petroleum::validate_syscall_fd(fd)?;

    if fd == 0 {
        if count == 1 {
            if let Some(ch) = nitrogen::ps2::keyboard::read_char() {
                let kernel_buf = [ch];
                unsafe { slice.copy_to_user(&kernel_buf) }
                    .map_err(|_| SyscallError::InvalidArgument)?;
                Ok(1)
            } else {
                Ok(0)
            }
        } else {
            let mut kernel_buf = vec![0u8; count];
            let bytes_read = nitrogen::ps2::keyboard::drain_line_buffer(&mut kernel_buf);
            unsafe { slice.copy_to_user(&kernel_buf[..bytes_read]) }
                .map_err(|_| SyscallError::InvalidArgument)?;
            Ok(bytes_read as u64)
        }
    } else {
        if fd < 0 {
            return Err(SyscallError::BadFileDescriptor);
        }
        with_current_fd_table(|table| match table.entries.get_mut(&(fd as u32)) {
            Some(file_desc) => {
                let mut kernel_buf = vec![0u8; count];
                match crate::fs::read_file(file_desc, &mut kernel_buf) {
                    Ok(bytes_read) => {
                        unsafe { slice.copy_to_user(&kernel_buf[..bytes_read]) }
                            .map_err(|_| SyscallError::InvalidArgument)?;
                        Ok(bytes_read as u64)
                    }
                    Err(_) => Err(SyscallError::BadFileDescriptor),
                }
            }
            None => Err(SyscallError::BadFileDescriptor),
        })
    }
}

pub(crate) fn syscall_write(fd: c_int, buffer: *const u8, count: usize) -> SyscallResult {
    petroleum::validate_syscall_fd(fd)?;
    let count = count.min(MAX_IO_BYTES);
    if count == 0 {
        return Ok(0);
    }

    let slice = UserSlice::new(buffer as *mut u8, count, false)
        .map_err(|_| SyscallError::InvalidArgument)?;

    let mut kernel_buf = vec![0u8; count];
    unsafe { slice.copy_from_user(&mut kernel_buf) }.map_err(|_| SyscallError::InvalidArgument)?;

    if fd == 1 || fd == 2 {
        petroleum::write_serial_bytes(0x3F8, 0x3FD, &kernel_buf);
        Ok(count as u64)
    } else {
        Err(SyscallError::BadFileDescriptor)
    }
}

pub(crate) fn syscall_open(filename: *const u8, flags: c_int, _mode: u32) -> SyscallResult {
    let filename = unsafe { copy_user_string(filename, MAX_PATH_BYTES)? };

    let read_only = (flags & 0x3) == O_RDONLY;
    let write_only = (flags & 0x3) == O_WRONLY;
    let read_write = (flags & 0x3) == O_RDWR;
    let create = (flags & O_CREAT) != 0;
    let truncate = (flags & O_TRUNC) != 0;
    let append = (flags & O_APPEND) != 0;

    if create || truncate || append || write_only || read_write {
        return Err(SyscallError::PermissionDenied);
    }

    if !read_only {
        return Err(SyscallError::PermissionDenied);
    }

    match crate::fs::open_file(&filename) {
        Ok(file_desc) => with_current_fd_table(|table| {
            let fd = table
                .alloc(file_desc)
                .map_err(|_| SyscallError::OutOfMemory)?;
            Ok(fd as u64)
        }),
        Err(crate::fs::FsError::FileNotFound) => Err(SyscallError::FileNotFound),
        Err(_) => Err(SyscallError::PermissionDenied),
    }
}

pub(crate) fn syscall_close(fd: c_int) -> SyscallResult {
    if fd <= 2 {
        return Err(SyscallError::InvalidArgument);
    }
    with_current_fd_table(|table| match table.entries.remove(&(fd as u32)) {
        Some(file_desc) => match crate::fs::close_file(file_desc) {
            Ok(_) => Ok(0),
            Err(_) => Err(SyscallError::BadFileDescriptor),
        },
        None => Err(SyscallError::BadFileDescriptor),
    })
}
