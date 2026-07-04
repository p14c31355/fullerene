use alloc::string::String;
use alloc::vec::Vec;
use petroleum::common::memory::UserSlice;
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
    if max_len == 0 {
        return Err(SyscallError::InvalidArgument);
    }

    // Validate the entire range via UserSlice
    let slice = UserSlice::new(ptr as *mut u8, max_len)
        .map_err(|_| SyscallError::InvalidArgument)?;

    // Copy into kernel-owned buffer
    let mut buf = vec![0u8; max_len];
    unsafe { slice.copy_from_user(&mut buf) }
        .map_err(|_| SyscallError::InvalidArgument)?;

    // Find NUL terminator (or use the full buffer)
    let actual_len = buf.iter().position(|&b| b == 0).unwrap_or(max_len);
    buf.truncate(actual_len);

    String::from_utf8(buf).map_err(|_| SyscallError::InvalidArgument)
}
