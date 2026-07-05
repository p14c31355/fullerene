use alloc::string::String;
use petroleum::common::memory::UserSlice;

#[cfg(test)]
use fullerene_abi::syscall_errors;

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

/// Helper function to safely copy a null-terminated string from user space.
///
/// Uses `UserSlice` for validated access.  The entire max_len range is
/// validated first, then bytes are scanned for a NUL terminator.
///
/// Returns the string if successful, or an error if validation fails.
///
/// # Safety
///
/// The caller must ensure the user pages are mapped.  Page faults during
/// copy are caught by the kernel's page fault handler.
pub unsafe fn copy_user_string(ptr: *const u8, max_len: usize) -> Result<String, SyscallError> {
    if ptr.is_null() || max_len == 0 {
        return Err(SyscallError::InvalidArgument);
    }

    let mut buf = alloc::vec::Vec::with_capacity(max_len.min(256));
    let mut offset = 0;

    while offset < max_len {
        let current = ptr.wrapping_add(offset);

        // Validate page on first access or when crossing a page boundary,
        // so a valid NUL-terminated string near an unmapped page works.
        if offset == 0 || ((current as usize) & 0xFFF) == 0 {
            let bytes_left_in_page = 4096 - ((current as usize) & 0xFFF);
            let remaining = (max_len - offset).min(bytes_left_in_page);
            let _ = UserSlice::new(current as *mut u8, remaining, false)
                .map_err(|_| SyscallError::InvalidArgument)?;
        }

        let byte = unsafe { core::ptr::read_volatile(current) };
        if byte == 0 {
            break;
        }
        buf.push(byte);
        offset += 1;
    }

    String::from_utf8(buf).map_err(|_| SyscallError::InvalidArgument)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_syscall_error_values() {
        assert_eq!(SyscallError::InvalidSyscall as i64, syscall_errors::INVALID_SYSCALL);
        assert_eq!(SyscallError::FileNotFound as i64, syscall_errors::FILE_NOT_FOUND);
        assert_eq!(SyscallError::NoSuchProcess as i64, syscall_errors::NO_SUCH_PROCESS);
        assert_eq!(SyscallError::BadFileDescriptor as i64, syscall_errors::BAD_FILE_DESCRIPTOR);
        assert_eq!(SyscallError::Again as i64, syscall_errors::AGAIN);
        assert_eq!(SyscallError::OutOfMemory as i64, syscall_errors::OUT_OF_MEMORY);
        assert_eq!(SyscallError::PermissionDenied as i64, syscall_errors::PERMISSION_DENIED);
        assert_eq!(SyscallError::AlreadyExists as i64, syscall_errors::ALREADY_EXISTS);
        assert_eq!(SyscallError::NoSuchDevice as i64, syscall_errors::NO_SUCH_DEVICE);
        assert_eq!(SyscallError::InvalidArgument as i64, syscall_errors::INVALID_ARGUMENT);
        assert_eq!(SyscallError::NotSupported as i64, syscall_errors::NOT_SUPPORTED);
        assert_eq!(SyscallError::BadHandle as i64, syscall_errors::BAD_HANDLE);
        assert_eq!(SyscallError::TimedOut as i64, syscall_errors::TIMED_OUT);
        assert_eq!(SyscallError::WouldBlock as i64, syscall_errors::WOULD_BLOCK);
    }
}
