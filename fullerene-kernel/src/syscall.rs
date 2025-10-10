//! System call interface for Fullerene OS
//!
//! This module provides the interface between user-space programs and kernel services.
//! System calls are invoked using interrupt 0x80 with the syscall number in EAX.

#![no_std]

use crate::process;
use core::ffi::c_int;

/// Helper function for serial port writes (from main.rs)
unsafe fn write_serial_bytes(port: u16, status_port: u16, bytes: &[u8]) {
    for &byte in bytes {
        // Wait for the serial port to be ready
        while (core::ptr::read_volatile(status_port as *const u8) & 0x20) == 0 {}
        // Write the byte
        core::ptr::write_volatile(port as *mut u8, byte);
    }
}

/// System call numbers
#[repr(u64)]
#[derive(Debug, Clone, Copy)]
pub enum SyscallNumber {
    /// Exit the current process (exit_code in EBX)
    Exit = 1,
    /// Write to file descriptor (fd in EBX, buffer in ECX, count in EDX)
    Write = 4,
    /// Open file (filename in EBX, flags in ECX, mode in EDX)
    Open = 5,
    /// Close file descriptor (fd in EBX)
    Close = 6,
    /// Read from file descriptor (fd in EBX, buffer in ECX, count in EDX)
    Read = 3,
    /// Create a new process (entry_point in EBX)
    Fork = 2,
    /// Wait for process to finish (pid in EBX)
    Wait = 7,
    /// Get current process ID
    GetPid = 20,
    /// Get process name (buffer in EBX, size in ECX)
    GetProcessName = 21,
    /// Yield to scheduler
    Yield = 22,
}

/// System call result type
pub type SyscallResult = Result<u64, SyscallError>;

/// System call errors
#[derive(Debug, Clone, Copy)]
pub enum SyscallError {
    /// Invalid system call number
    InvalidSyscall = 1,
    /// Invalid file descriptor
    BadFileDescriptor = 9,
    /// Permission denied
    PermissionDenied = 13,
    /// File not found
    FileNotFound = 2,
    /// No such process
    NoSuchProcess = 3,
    /// Invalid argument
    InvalidArgument = 22,
    /// Out of memory
    OutOfMemory = 12,
}

/// Handle system call from user space
///
/// This function is called from the syscall interrupt handler
/// and dispatches to the appropriate system call handler.
///
/// # Arguments
/// * `syscall_num` - The system call number
/// * `arg1` - First argument (EBX)
/// * `arg2` - Second argument (ECX)
/// * `arg3` - Third argument (EDX)
/// * `arg4` - Fourth argument (ESI)
/// * `arg5` - Fifth argument (EDI)
///
/// # Returns
/// Result of the system call in EAX
pub unsafe extern "C" fn handle_syscall(
    syscall_num: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    _arg4: u64,
    _arg5: u64,
) -> u64 {
    let result = match syscall_num {
        1 => syscall_exit(arg1 as i32),
        2 => syscall_fork(),
        3 => syscall_read(arg1 as c_int, arg2 as *mut u8, arg3 as usize),
        4 => syscall_write(arg1 as c_int, arg2 as *const u8, arg3 as usize),
        5 => syscall_open(arg1 as *const u8, arg2 as c_int, arg3 as u32),
        6 => syscall_close(arg1 as c_int),
        7 => syscall_wait(arg1 as u64),
        20 => syscall_getpid(),
        21 => syscall_get_process_name(arg1 as *mut u8, arg2 as usize),
        22 => syscall_yield(),
        _ => Err(SyscallError::InvalidSyscall),
    };

    match result {
        Ok(value) => value,
        Err(error) => -(error as i32) as u64, // Negative values indicate errors
    }
}

/// Exit system call
fn syscall_exit(exit_code: i32) -> SyscallResult {
    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    process::terminate_process(pid, exit_code);
    Ok(0)
}

/// Fork system call - creates a new process
/// For simplicity, this creates a new process with the same entry point
fn syscall_fork() -> SyscallResult {
    // In a real implementation, this would clone the current process
    // For now, create a dummy process
    let pid = process::create_process("forked_process", || {
        // Dummy entry point - just exit
        process::terminate_process(process::current_pid().unwrap_or(0), 0);
    });
    Ok(pid)
}

/// Read system call
fn syscall_read(fd: c_int, buffer: *mut u8, count: usize) -> SyscallResult {
    if fd < 0 || buffer.is_null() {
        return Err(SyscallError::InvalidArgument);
    }

    // For now, only support reading from stdin (fd 0)
    // Return 0 (EOF) for simplicity
    Ok(0)
}

/// Write system call
fn syscall_write(fd: c_int, buffer: *const u8, count: usize) -> SyscallResult {
    if fd < 0 || buffer.is_null() {
        return Err(SyscallError::InvalidArgument);
    }

    // Check if buffer is valid
    if count == 0 {
        return Ok(0);
    }

    // Create a slice from the buffer pointer
    let data = unsafe { core::slice::from_raw_parts(buffer, count) };

    // For stdout (fd 1) and stderr (fd 2), write to serial console
    if fd == 1 || fd == 2 {
        unsafe {
            write_serial_bytes(0x3F8, 0x3FD, data);
        }
        Ok(count as u64)
    } else {
        Err(SyscallError::BadFileDescriptor)
    }
}

/// Open system call
fn syscall_open(filename: *const u8, _flags: c_int, _mode: u32) -> SyscallResult {
    if filename.is_null() {
        return Err(SyscallError::InvalidArgument);
    }

    // Convert filename to string
    let mut len = 0;
    unsafe {
        while *filename.add(len) != 0 {
            len += 1;
            if len > 256 {
                // Reasonable limit
                return Err(SyscallError::InvalidArgument);
            }
        }
        let filename_slice = core::slice::from_raw_parts(filename, len);
        let filename_str =
            core::str::from_utf8(filename_slice).map_err(|_| SyscallError::InvalidArgument)?;
    }

    // For now, always fail (no filesystem yet)
    Err(SyscallError::FileNotFound)
}

/// Close system call
fn syscall_close(_fd: c_int) -> SyscallResult {
    // For now, always succeed
    Ok(0)
}

/// Wait system call
fn syscall_wait(_pid: u64) -> SyscallResult {
    // For now, just yield
    process::yield_current();
    Ok(0)
}

/// Get process ID
fn syscall_getpid() -> SyscallResult {
    Ok(process::current_pid().unwrap_or(0))
}

/// Get process name
fn syscall_get_process_name(buffer: *mut u8, size: usize) -> SyscallResult {
    if buffer.is_null() || size == 0 {
        return Err(SyscallError::InvalidArgument);
    }

    // For now, return empty string
    Ok(0)
}

/// Yield system call
fn syscall_yield() -> SyscallResult {
    process::yield_current();
    Ok(0)
}

/// Initialize system calls
pub fn init() {
    // Add syscall interrupt handler to IDT
    // This would normally be done in interrupts::init()
    // For now, assume it's handled there
}

/// Syscall helper macros for user space (would be in user-space library)
#[cfg(feature = "user_space")]
pub mod user {
    use super::SyscallNumber;

    /// Make a system call (user space wrapper)
    #[inline(always)]
    pub unsafe fn syscall(syscall_num: SyscallNumber, arg1: u64, arg2: u64, arg3: u64) -> u64 {
        let mut result: u64;
        asm!(
            "int 0x80",
            in("rax") syscall_num as u64,
            in("rbx") arg1,
            in("rcx") arg2,
            in("rdx") arg3,
            lateout("rax") result,
        );
        result
    }

    /// Exit wrapper
    pub fn exit(code: i32) -> ! {
        unsafe { syscall(SyscallNumber::Exit, code as u64, 0, 0) };
        loop {} // Should not reach here
    }

    /// Write wrapper
    pub fn write(fd: i32, buf: &[u8]) -> i64 {
        unsafe {
            syscall(
                SyscallNumber::Write,
                fd as u64,
                buf.as_ptr() as u64,
                buf.len() as u64,
            ) as i64
        }
    }

    /// Get PID wrapper
    pub fn getpid() -> u64 {
        unsafe { syscall(SyscallNumber::GetPid, 0, 0, 0) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_syscall_numbers() {
        assert_eq!(SyscallNumber::Exit as u64, 1);
        assert_eq!(SyscallNumber::Write as u64, 4);
        assert_eq!(SyscallNumber::Read as u64, 3);
    }

    #[test]
    fn test_exit_syscall() {
        // This would normally exit, but in test we can't
        let result = syscall_exit(0);
        assert!(result.is_ok());
    }
}
