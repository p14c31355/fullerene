use alloc::string::String;
use fullerene_abi::SyscallErrorCode;
use petroleum::common::logging::SystemError;
use petroleum::common::memory::UserSlice;

use crate::user_memory::{self, UserCopyError};

#[cfg(test)]
use fullerene_abi::syscall_errors;

/// System call result type
pub type SyscallResult = Result<u64, SyscallError>;

/// System call errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum SyscallError {
    /// Invalid system call number
    InvalidSyscall = SyscallErrorCode::InvalidSyscall as i64,
    /// File not found
    FileNotFound = SyscallErrorCode::FileNotFound as i64,
    /// No such process
    NoSuchProcess = SyscallErrorCode::NoSuchProcess as i64,
    /// Device or block I/O error
    Io = SyscallErrorCode::Io as i64,
    /// Bad file descriptor
    BadFileDescriptor = SyscallErrorCode::BadFileDescriptor as i64,
    /// Out of memory
    OutOfMemory = SyscallErrorCode::OutOfMemory as i64,
    /// Permission denied
    PermissionDenied = SyscallErrorCode::PermissionDenied as i64,
    /// Invalid or inaccessible address
    AddressFault = SyscallErrorCode::AddressFault as i64,
    /// Resource or device is busy
    Busy = SyscallErrorCode::Busy as i64,
    /// Invalid argument
    InvalidArgument = SyscallErrorCode::InvalidArgument as i64,
    /// Resource temporarily unavailable (try again)
    Again = SyscallErrorCode::Again as i64,
    /// Operation timed out
    TimedOut = SyscallErrorCode::TimedOut as i64,
    /// Operation not supported
    NotSupported = SyscallErrorCode::NotSupported as i64,
    /// Resource already exists
    AlreadyExists = SyscallErrorCode::AlreadyExists as i64,
    /// No such device
    NoSuchDevice = SyscallErrorCode::NoSuchDevice as i64,
    /// Expected a directory
    NotADirectory = SyscallErrorCode::NotADirectory as i64,
    /// Expected a non-directory object
    IsADirectory = SyscallErrorCode::IsADirectory as i64,
    /// Storage capacity exhausted
    NoSpace = SyscallErrorCode::NoSpace as i64,
    /// Directory must be empty
    DirectoryNotEmpty = SyscallErrorCode::DirectoryNotEmpty as i64,
    /// Numeric or address overflow
    Overflow = SyscallErrorCode::Overflow as i64,
    /// Bad handle
    BadHandle = SyscallErrorCode::BadHandle as i64,
    /// Operation would block
    WouldBlock = SyscallErrorCode::WouldBlock as i64,
}

petroleum::error_chain!(SyscallError, petroleum::common::logging::SystemError,
    SyscallError::InvalidSyscall => petroleum::common::logging::SystemError::InvalidSyscall,
    SyscallError::BadFileDescriptor => petroleum::common::logging::SystemError::BadFileDescriptor,
    SyscallError::PermissionDenied => petroleum::common::logging::SystemError::PermissionDenied,
    SyscallError::FileNotFound => petroleum::common::logging::SystemError::FileNotFound,
    SyscallError::NoSuchProcess => petroleum::common::logging::SystemError::NoSuchProcess,
    SyscallError::Io => petroleum::common::logging::SystemError::DeviceError,
    SyscallError::InvalidArgument => petroleum::common::logging::SystemError::InvalidArgument,
    SyscallError::OutOfMemory => petroleum::common::logging::SystemError::SyscallOutOfMemory,
    SyscallError::AddressFault => petroleum::common::logging::SystemError::MappingFailed,
    SyscallError::Busy => petroleum::common::logging::SystemError::OperationAgain,
    SyscallError::Again => petroleum::common::logging::SystemError::OperationAgain,
    SyscallError::TimedOut => petroleum::common::logging::SystemError::OperationTimedOut,
    SyscallError::NotSupported => petroleum::common::logging::SystemError::NotSupported,
    SyscallError::AlreadyExists => petroleum::common::logging::SystemError::FileExists,
    SyscallError::NoSuchDevice => petroleum::common::logging::SystemError::NoSuchDevice,
    SyscallError::NotADirectory => petroleum::common::logging::SystemError::InvalidArgument,
    SyscallError::IsADirectory => petroleum::common::logging::SystemError::InvalidArgument,
    SyscallError::NoSpace => petroleum::common::logging::SystemError::DiskFull,
    SyscallError::DirectoryNotEmpty => petroleum::common::logging::SystemError::InvalidArgument,
    SyscallError::Overflow => petroleum::common::logging::SystemError::InvalidArgument,
    SyscallError::BadHandle => petroleum::common::logging::SystemError::BadHandle,
    SyscallError::WouldBlock => petroleum::common::logging::SystemError::WouldBlock,
);

impl From<petroleum::common::logging::SystemError> for SyscallError {
    fn from(error: petroleum::common::logging::SystemError) -> Self {
        use petroleum::common::logging::SystemError;
        match error {
            SystemError::InvalidSyscall => Self::InvalidSyscall,
            SystemError::BadFileDescriptor | SystemError::FsInvalidFileDescriptor => {
                Self::BadFileDescriptor
            }
            SystemError::PermissionDenied => Self::PermissionDenied,
            SystemError::FileNotFound => Self::FileNotFound,
            SystemError::NoSuchProcess => Self::NoSuchProcess,
            SystemError::InvalidArgument
            | SystemError::InvalidSeek
            | SystemError::UnmappingFailed
            | SystemError::InvalidFormat
            | SystemError::LoadFailed
            | SystemError::InternalError
            | SystemError::UnknownError => Self::InvalidArgument,
            SystemError::SyscallOutOfMemory
            | SystemError::FrameAllocationFailed
            | SystemError::MemOutOfMemory => Self::OutOfMemory,
            SystemError::FileExists => Self::AlreadyExists,
            SystemError::DiskFull => Self::NoSpace,
            SystemError::MappingFailed => Self::AddressFault,
            SystemError::DeviceNotFound | SystemError::NoSuchDevice => Self::NoSuchDevice,
            SystemError::DeviceError | SystemError::PortError => Self::Io,
            SystemError::NotImplemented | SystemError::NotSupported => Self::NotSupported,
            SystemError::TooManyProcesses | SystemError::OperationAgain => Self::Again,
            SystemError::OperationTimedOut => Self::TimedOut,
            SystemError::BadHandle => Self::BadHandle,
            SystemError::WouldBlock => Self::WouldBlock,
        }
    }
}

impl From<genome::fs::FsError> for SyscallError {
    fn from(error: genome::fs::FsError) -> Self {
        use genome::fs::FsError;
        match error {
            FsError::FileNotFound => Self::FileNotFound,
            FsError::FileExists => Self::AlreadyExists,
            FsError::PermissionDenied => Self::PermissionDenied,
            FsError::InvalidFileDescriptor => Self::BadFileDescriptor,
            FsError::InvalidSeek | FsError::InvalidPath | FsError::InvalidInput => {
                Self::InvalidArgument
            }
            FsError::DiskFull => Self::NoSpace,
            FsError::NotADirectory => Self::NotADirectory,
            FsError::DirectoryNotEmpty => Self::DirectoryNotEmpty,
            FsError::IsADirectory => Self::IsADirectory,
            FsError::NotSupported => Self::NotSupported,
            FsError::UnexpectedEof => Self::Io,
            FsError::Io => Self::Io,
        }
    }
}

impl From<genome::block::BlockError> for SyscallError {
    fn from(error: genome::block::BlockError) -> Self {
        use genome::block::BlockError;
        match error {
            BlockError::Device => Self::Io,
            BlockError::BufferTooSmall { .. } => Self::InvalidArgument,
            BlockError::LbaOverflow => Self::Overflow,
            BlockError::SectorNotFound => Self::FileNotFound,
        }
    }
}

impl From<nitrogen::DriverError> for SyscallError {
    fn from(error: nitrogen::DriverError) -> Self {
        use nitrogen::DriverError;
        match error {
            DriverError::DeviceNotFound => Self::NoSuchDevice,
            DriverError::NotReady => Self::Again,
            DriverError::InvalidArgument => Self::InvalidArgument,
            DriverError::OutOfMemory => Self::OutOfMemory,
            DriverError::MmioMappingFailed => Self::OutOfMemory,
            DriverError::DmaMappingFailed
            | DriverError::Io
            | DriverError::Protocol
            | DriverError::DeviceFault => Self::Io,
            DriverError::TimedOut => Self::TimedOut,
            DriverError::Busy => Self::Busy,
            DriverError::NotSupported => Self::NotSupported,
        }
    }
}

impl From<petroleum::MemoryError> for SyscallError {
    fn from(error: petroleum::MemoryError) -> Self {
        use petroleum::MemoryError;
        match error {
            MemoryError::OutOfMemory | MemoryError::FrameAllocationFailed => Self::OutOfMemory,
            MemoryError::MappingFailed | MemoryError::InvalidAddress => Self::AddressFault,
            MemoryError::UnmappingFailed
            | MemoryError::InvalidAlignment
            | MemoryError::InvalidSize
            | MemoryError::NotMapped => Self::InvalidArgument,
            MemoryError::AddressOverflow => Self::Overflow,
            MemoryError::AlreadyMapped => Self::AlreadyExists,
            MemoryError::PermissionDenied => Self::PermissionDenied,
            MemoryError::NotInitialized => Self::AddressFault,
        }
    }
}

fn versioned_copy_len(
    caller_size: usize,
    minimum_size: usize,
    current_size: usize,
) -> Result<usize, SyscallError> {
    if minimum_size == 0 || current_size < minimum_size || caller_size < minimum_size {
        return Err(SyscallError::InvalidArgument);
    }
    Ok(caller_size.min(current_size))
}

/// Copy the compatible prefix of a versioned DTO to a validated user buffer.
pub(crate) fn copy_versioned_dto_to_user(
    destination: *mut u8,
    caller_size: usize,
    minimum_size: usize,
    bytes: &[u8],
) -> SyscallResult {
    if destination.is_null() {
        return Err(SyscallError::InvalidArgument);
    }

    let copy_len = versioned_copy_len(caller_size, minimum_size, bytes.len())?;
    petroleum::validate_user_buffer(destination as usize, copy_len, false)
        .map_err(|_| SyscallError::AddressFault)?;
    let destination =
        UserSlice::new(destination, copy_len, true).map_err(|_| SyscallError::AddressFault)?;
    unsafe { destination.copy_to_user(&bytes[..copy_len]) }
        .map_err(|_| SyscallError::AddressFault)?;
    Ok(copy_len as u64)
}

/// Helper function to safely copy a null-terminated string from user space.
///
/// Uses the shared user-memory implementation for page-wise validation and
/// copies into a kernel-owned buffer before decoding UTF-8.
///
/// Returns the string if successful, or an error if validation fails.
///
/// # Safety
///
/// The caller must ensure the user pages are mapped.  Page faults during
/// copy are caught by the kernel's page fault handler.
pub unsafe fn copy_user_string(ptr: *const u8, max_len: usize) -> Result<String, SyscallError> {
    unsafe { user_memory::copy_c_string(ptr, max_len) }.map_err(|error| match error {
        UserCopyError::System(SystemError::MemOutOfMemory | SystemError::SyscallOutOfMemory) => {
            SyscallError::OutOfMemory
        }
        UserCopyError::System(_) | UserCopyError::InvalidUtf8 | UserCopyError::MissingNul => {
            SyscallError::InvalidArgument
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_syscall_error_values() {
        assert_eq!(
            SyscallError::InvalidSyscall as i64,
            syscall_errors::INVALID_SYSCALL
        );
        assert_eq!(
            SyscallError::FileNotFound as i64,
            syscall_errors::FILE_NOT_FOUND
        );
        assert_eq!(
            SyscallError::NoSuchProcess as i64,
            syscall_errors::NO_SUCH_PROCESS
        );
        assert_eq!(SyscallError::Io as i64, syscall_errors::IO_ERROR);
        assert_eq!(
            SyscallError::BadFileDescriptor as i64,
            syscall_errors::BAD_FILE_DESCRIPTOR
        );
        assert_eq!(SyscallError::Again as i64, syscall_errors::AGAIN);
        assert_eq!(
            SyscallError::OutOfMemory as i64,
            syscall_errors::OUT_OF_MEMORY
        );
        assert_eq!(
            SyscallError::PermissionDenied as i64,
            syscall_errors::PERMISSION_DENIED
        );
        assert_eq!(
            SyscallError::AddressFault as i64,
            syscall_errors::ADDRESS_FAULT
        );
        assert_eq!(SyscallError::Busy as i64, syscall_errors::BUSY);
        assert_eq!(
            SyscallError::AlreadyExists as i64,
            syscall_errors::ALREADY_EXISTS
        );
        assert_eq!(
            SyscallError::NoSuchDevice as i64,
            syscall_errors::NO_SUCH_DEVICE
        );
        assert_eq!(
            SyscallError::NotADirectory as i64,
            syscall_errors::NOT_A_DIRECTORY
        );
        assert_eq!(
            SyscallError::IsADirectory as i64,
            syscall_errors::IS_A_DIRECTORY
        );
        assert_eq!(SyscallError::NoSpace as i64, syscall_errors::NO_SPACE);
        assert_eq!(
            SyscallError::DirectoryNotEmpty as i64,
            syscall_errors::DIRECTORY_NOT_EMPTY
        );
        assert_eq!(SyscallError::Overflow as i64, syscall_errors::OVERFLOW);
        assert_eq!(
            SyscallError::InvalidArgument as i64,
            syscall_errors::INVALID_ARGUMENT
        );
        assert_eq!(
            SyscallError::NotSupported as i64,
            syscall_errors::NOT_SUPPORTED
        );
        assert_eq!(SyscallError::BadHandle as i64, syscall_errors::BAD_HANDLE);
        assert_eq!(SyscallError::TimedOut as i64, syscall_errors::TIMED_OUT);
        assert_eq!(SyscallError::WouldBlock as i64, syscall_errors::WOULD_BLOCK);
    }

    #[test]
    fn versioned_dto_copy_length_accepts_older_buffers() {
        assert_eq!(versioned_copy_len(40, 40, 48), Ok(40));
        assert_eq!(versioned_copy_len(48, 40, 48), Ok(48));
        assert_eq!(versioned_copy_len(64, 40, 48), Ok(48));
        assert_eq!(
            versioned_copy_len(39, 40, 48),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn domain_errors_preserve_native_syscall_meaning() {
        assert_eq!(
            SyscallError::from(genome::fs::FsError::DirectoryNotEmpty),
            SyscallError::DirectoryNotEmpty
        );
        assert_eq!(
            SyscallError::from(genome::block::BlockError::LbaOverflow),
            SyscallError::Overflow
        );
        assert_eq!(
            SyscallError::from(nitrogen::DriverError::TimedOut),
            SyscallError::TimedOut
        );
        assert_eq!(
            SyscallError::from(petroleum::MemoryError::AlreadyMapped),
            SyscallError::AlreadyExists
        );
        assert_eq!(
            SyscallError::from(petroleum::MemoryError::MappingFailed),
            SyscallError::AddressFault
        );
        assert_eq!(
            SyscallError::from(petroleum::SystemError::MappingFailed),
            SyscallError::AddressFault
        );
        assert_eq!(
            SyscallError::from(petroleum::SystemError::SyscallOutOfMemory),
            SyscallError::OutOfMemory
        );
        assert_eq!(
            SyscallError::from(petroleum::SystemError::WouldBlock),
            SyscallError::WouldBlock
        );
        assert_eq!(
            SyscallError::from(genome::fs::FsError::Io),
            SyscallError::Io
        );
        assert_eq!(
            SyscallError::from(petroleum::MemoryError::NotInitialized),
            SyscallError::AddressFault
        );
    }
}
