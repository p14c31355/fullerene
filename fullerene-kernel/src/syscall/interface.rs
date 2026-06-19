use alloc::string::String;
use alloc::vec::Vec;
use x86_64::VirtAddr;

/// System call result type
pub type SyscallResult = Result<u64, SyscallError>;

/// System call errors
#[derive(Debug, Clone, Copy)]
pub enum SyscallError {
    /// Invalid system call number
    InvalidSyscall = 1,
    /// File not found
    FileNotFound = 2,
    /// No such process
    NoSuchProcess = 3,
    /// Bad file descriptor
    BadFileDescriptor = 9,
    /// Out of memory
    OutOfMemory = 12,
    /// Permission denied
    PermissionDenied = 13,
    /// Invalid argument
    InvalidArgument = 22,
    /// Resource temporarily unavailable (try again)
    Again = 11,
    /// Operation timed out
    TimedOut = 110,
    /// Operation not supported
    NotSupported = 95,
    /// Resource already exists
    AlreadyExists = 17,
    /// No such device
    NoSuchDevice = 19,
    /// Bad handle
    BadHandle = 104,
    /// Operation would block
    WouldBlock = 140,
}

petroleum::error_chain!(SyscallError, petroleum::common::logging::SystemError,
    SyscallError::InvalidSyscall => petroleum::common::logging::SystemError::InvalidSyscall,
    SyscallError::BadFileDescriptor => petroleum::common::logging::SystemError::BadFileDescriptor,
    SyscallError::PermissionDenied => petroleum::common::logging::SystemError::PermissionDenied,
    SyscallError::FileNotFound => petroleum::common::logging::SystemError::FileNotFound,
    SyscallError::NoSuchProcess => petroleum::common::logging::SystemError::NoSuchProcess,
    SyscallError::InvalidArgument => petroleum::common::logging::SystemError::InvalidArgument,
    SyscallError::OutOfMemory => petroleum::common::logging::SystemError::SyscallOutOfMemory,
    SyscallError::Again => petroleum::common::logging::SystemError::OperationAgain,
    SyscallError::TimedOut => petroleum::common::logging::SystemError::OperationTimedOut,
    SyscallError::NotSupported => petroleum::common::logging::SystemError::NotSupported,
    SyscallError::AlreadyExists => petroleum::common::logging::SystemError::FileExists,
    SyscallError::NoSuchDevice => petroleum::common::logging::SystemError::NoSuchDevice,
    SyscallError::BadHandle => petroleum::common::logging::SystemError::BadHandle,
    SyscallError::WouldBlock => petroleum::common::logging::SystemError::WouldBlock,
);

impl From<petroleum::common::logging::SystemError> for SyscallError {
    fn from(error: petroleum::common::logging::SystemError) -> Self {
        match error {
            petroleum::common::logging::SystemError::MemOutOfMemory => SyscallError::OutOfMemory,
            petroleum::common::logging::SystemError::InvalidArgument => {
                SyscallError::InvalidArgument
            }
            petroleum::common::logging::SystemError::PermissionDenied => {
                SyscallError::PermissionDenied
            }
            petroleum::common::logging::SystemError::FileNotFound => SyscallError::FileNotFound,
            petroleum::common::logging::SystemError::NoSuchProcess => SyscallError::NoSuchProcess,
            petroleum::common::logging::SystemError::BadFileDescriptor => {
                SyscallError::BadFileDescriptor
            }
            // Default to InvalidArgument for unhandled errors
            _ => SyscallError::InvalidArgument,
        }
    }
}

/// Helper function to safely copy a null-terminated string from user space
/// Returns the string if successful, or an error if validation fails
///
/// # Safety
///
/// The caller must ensure that the pointer `ptr` is valid and the memory range
/// being read does not violate any memory safety constraints.
pub unsafe fn copy_user_string(ptr: *const u8, max_len: usize) -> Result<String, SyscallError> {
    if ptr.is_null() {
        return Err(SyscallError::InvalidArgument);
    }

    // Validate that the initial pointer range is in user space
    let start_addr = VirtAddr::new(ptr as u64);
    if !petroleum::is_user_address(start_addr) {
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
            if !petroleum::is_user_address(addr) {
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
