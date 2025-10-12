use crate::process;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use x86_64::VirtAddr;

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

/// Helper function to validate user buffer access
pub fn validate_user_buffer(
    ptr: usize,
    count: usize,
    allow_kernel: bool,
) -> Result<(), SyscallError> {
    use crate::memory_management::is_user_address;
    use x86_64::VirtAddr;

    if ptr == 0 && count == 0 {
        return Ok(());
    }

    let start = VirtAddr::new(ptr as u64);
    if !allow_kernel && !is_user_address(start) {
        return Err(SyscallError::InvalidArgument);
    }

    if count == 0 {
        return Ok(());
    }

    if let Some(end_ptr) = ptr.checked_add(count - 1) {
        let end = VirtAddr::new(end_ptr as u64);
        if !allow_kernel && !is_user_address(end) {
            return Err(SyscallError::InvalidArgument);
        }
    } else {
        return Err(SyscallError::InvalidArgument);
    }

    Ok(())
}

/// Helper function to safely copy a null-terminated string from user space
/// Returns the string if successful, or an error if validation fails
pub fn copy_user_string(ptr: *const u8, max_len: usize) -> Result<String, SyscallError> {
    if ptr.is_null() {
        return Err(SyscallError::InvalidArgument);
    }

    // Validate that the initial pointer range is in user space
    use crate::memory_management::is_user_address;
    let start_addr = VirtAddr::new(ptr as u64);
    if !is_user_address(start_addr) {
        return Err(SyscallError::InvalidArgument);
    }

    // Note: In a real implementation, we would need to handle page faults
    // when accessing user memory from kernel mode. For now, assume the memory
    // is mapped and accessible.

    let mut len = 0;
    let mut buffer = Vec::new();

    // Copy bytes one by one, validating each address
    while len < max_len {
        // Check if current pointer is in user space
        if let Some(next_addr) = (ptr as u64).checked_add(len as u64) {
            let addr = VirtAddr::new(next_addr);
            if !is_user_address(addr) {
                return Err(SyscallError::InvalidArgument);
            }
        } else {
            return Err(SyscallError::InvalidArgument);
        }

        // Read the byte safely
        let byte = unsafe { ptr.add(len).read() };
        if byte == 0 {
            break; // Null terminator found
        }
        buffer.push(byte);
        len += 1;

        // Prevent infinite loops on malformed strings
        if len >= max_len {
            return Err(SyscallError::InvalidArgument);
        }
    }

    // Convert bytes to string
    String::from_utf8(buffer).map_err(|_| SyscallError::InvalidArgument)
}
