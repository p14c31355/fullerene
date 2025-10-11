use super::interface::{SyscallResult, SyscallError, validate_user_buffer, copy_user_string, SyscallNumber};
use crate::process;
use petroleum::write_serial_bytes;

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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn handle_syscall(
    syscall_num: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    _arg4: u64,
    _arg5: u64,
    _arg6: u64,
) -> u64 {
    let result = match syscall_num {
        1 => syscall_exit(arg1 as i32),
        2 => syscall_fork(),
        3 => syscall_read(arg1 as core::ffi::c_int, arg2 as *mut u8, arg3 as usize),
        4 => syscall_write(arg1 as core::ffi::c_int, arg2 as *const u8, arg3 as usize),
        5 => syscall_open(arg1 as *const u8, arg2 as core::ffi::c_int, arg3 as u32),
        6 => syscall_close(arg1 as core::ffi::c_int),
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

// Exit system call
pub(crate) fn syscall_exit(exit_code: i32) -> SyscallResult {
    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    process::terminate_process(pid, exit_code);
    Ok(0)
}

fn syscall_fork() -> SyscallResult {
    let current_pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;

    let process_list = crate::process::PROCESS_LIST.lock();
    let parent_process = process_list
        .iter()
        .find(|p| p.id == current_pid)
        .ok_or(SyscallError::NoSuchProcess)?;

    // Clone the parent process name (use parent name for now, full clone later)
    let child_name = "fork_child"; // Use static string to avoid lifetime issues

    // For now, create a basic child process
    // In a full implementation, we would:
    // 1. Clone the process memory space
    // 2. Copy the page tables
    // 3. Set up the child context to return 0 from fork
    // 4. Set up the parent to return the child PID

    let child_entry = parent_process.entry_point; // Same entry point for now
    drop(process_list); // Release lock

    let child_pid = process::create_process(child_name, child_entry);

    // TODO: Implement full process cloning with memory copying
    // For now, we return the child PID from fork (should return 0 in child, child_pid in parent)
    // This is a significant simplification - a real fork would require context switching

    Ok(child_pid)
}

/// Read system call
fn syscall_read(fd: core::ffi::c_int, buffer: *mut u8, count: usize) -> SyscallResult {
    if fd < 0 {
        return Err(SyscallError::InvalidArgument);
    }

    if buffer.is_null() {
        return Err(SyscallError::InvalidArgument);
    }

    // POSIX: reading 0 bytes should return 0 immediately
    if count == 0 {
        return Ok(0);
    }

    // Check if buffer is valid for user space
    validate_user_buffer(buffer as usize, count, false)?;

    // For now, only support reading from stdin (fd 0)
    if fd == 0 {
        // Read from keyboard input buffer
        let data = unsafe { core::slice::from_raw_parts_mut(buffer, count) };
        let bytes_read = crate::keyboard::drain_line_buffer(data);

        // Convert line ending if present
        if bytes_read > 0 && bytes_read <= count {
            let last_idx = bytes_read - 1;
            if data[last_idx] == b'\n' && last_idx + 1 < count {
                data[last_idx + 1] = b'\0'; // Add null terminator for C strings
            }
        }

        Ok(bytes_read as u64)
    } else {
        // Attempt to read from the file descriptor using fs module
        let data = unsafe { core::slice::from_raw_parts_mut(buffer, count) };
        match crate::fs::read_file(fd, data) {
            Ok(bytes_read) => Ok(bytes_read as u64),
            Err(crate::fs::FsError::InvalidFileDescriptor) => Err(SyscallError::BadFileDescriptor),
            Err(crate::fs::FsError::PermissionDenied) => Err(SyscallError::PermissionDenied),
            Err(_) => Err(SyscallError::FileNotFound),
        }
    }
}

/// Write system call
fn syscall_write(fd: core::ffi::c_int, buffer: *const u8, count: usize) -> SyscallResult {
    if fd < 0 || buffer.is_null() {
        return Err(SyscallError::InvalidArgument);
    }

    if count == 0 {
        return Ok(0);
    }

    // Validate that the buffer range is valid; allow kernel pointers for stdout/stderr
    let allow_kernel = fd == 1 || fd == 2;
    validate_user_buffer(buffer as usize, count, allow_kernel)?;

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
fn syscall_open(filename: *const u8, _flags: core::ffi::c_int, _mode: u32) -> SyscallResult {
    // Safely copy the filename from user space
    let filename_str = copy_user_string(filename, 256)?;

    // TODO: Use flags to determine open mode (read, write, create, etc.)
    // For now, attempt to open existing file
    match crate::fs::open_file(&filename_str) {
        Ok(fd) => Ok(fd as u64),
        Err(crate::fs::FsError::FileNotFound) => Err(SyscallError::FileNotFound),
        Err(_) => Err(SyscallError::PermissionDenied),
    }
}

/// Close system call
fn syscall_close(fd: core::ffi::c_int) -> SyscallResult {
    if fd < 0 {
        return Err(SyscallError::InvalidArgument);
    }

    // Attempt to close the file descriptor using fs module
    match crate::fs::close_file(fd) {
        Ok(()) => Ok(0),
        Err(crate::fs::FsError::InvalidFileDescriptor) => Err(SyscallError::BadFileDescriptor),
        Err(_) => Err(SyscallError::InvalidArgument),
    }
}

/// Wait system call
fn syscall_wait(pid: u64) -> SyscallResult {
    if pid == 0 {
        // Wait for any child process (not implemented yet)
        // For now, just yield
        process::yield_current();
        Ok(0)
    } else {
        // Wait for specific process to finish
        // Check if the process exists and is a child (simplified check)
        let process_list = crate::process::PROCESS_LIST.lock();
        if let Some(process) = process_list.iter().find(|p| p.id == pid) {
            if process.state == crate::process::ProcessState::Terminated {
                // Process has already finished, return exit code
                let exit_code = process.exit_code.unwrap_or(0);
                Ok(exit_code as u64)
            } else {
                // Process is still running, block current process
                drop(process_list); // Release lock
                crate::process::block_current();
                Ok(0)
            }
        } else {
            Err(SyscallError::NoSuchProcess)
        }
    }
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

    // Check if buffer is valid for user space
    validate_user_buffer(buffer as usize, size, false)?;

    let current_pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;

    let process_list = crate::process::PROCESS_LIST.lock();
    if let Some(process) = process_list.iter().find(|p| p.id == current_pid) {
        let name_bytes = process.name.as_bytes();
        let copy_len = name_bytes.len().min(size - 1); // Leave room for null terminator

        // Copy the process name to user buffer
        unsafe {
            core::ptr::copy_nonoverlapping(name_bytes.as_ptr(), buffer, copy_len);
            // Add null terminator
            *buffer.add(copy_len) = b'\0';
        }

        Ok(copy_len as u64)
    } else {
        Err(SyscallError::NoSuchProcess)
    }
}

/// Yield system call
fn syscall_yield() -> SyscallResult {
    process::yield_current();
    Ok(0)
}

/// Kernel syscall call - calls syscall handler directly without syscall overhead
/// This allows kernel code to call syscalls without the unnecessary hardware syscall overhead
pub fn kernel_syscall(syscall_num: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    unsafe { handle_syscall(syscall_num, arg1, arg2, arg3, 0, 0, 0) }
}

/// Initialize system calls
pub fn init() {
    // Add syscall interrupt handler to IDT
    // This would normally be done in interrupts::init()
    // For now, assume it's handled there
}
