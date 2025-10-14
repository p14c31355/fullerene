//! System error types and conversions

// Common error type for the entire system
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemError {
    // System call errors
    InvalidSyscall = 1,
    BadFileDescriptor = 9,
    PermissionDenied = 13,
    FileNotFound = 2,
    NoSuchProcess = 3,
    InvalidArgument = 22,
    SyscallOutOfMemory = 12,

    // File system errors
    FileExists = 17,
    FsInvalidFileDescriptor = 25,
    InvalidSeek = 29,
    DiskFull = 28,

    // Memory management errors
    MappingFailed = 100,
    UnmappingFailed = 101,
    FrameAllocationFailed = 102,
    MemOutOfMemory = 103,

    // Loader errors
    InvalidFormat = 200,
    LoadFailed = 201,

    // Hardware errors
    DeviceNotFound = 300,
    DeviceError = 301,
    PortError = 302,

    // General errors
    NotImplemented = 400,
    InternalError = 500,
    UnknownError = 999,
}

impl From<crate::syscall::interface::SyscallError> for SystemError {
    fn from(error: crate::syscall::interface::SyscallError) -> Self {
        match error {
            crate::syscall::interface::SyscallError::InvalidSyscall => SystemError::InvalidSyscall,
            crate::syscall::interface::SyscallError::BadFileDescriptor => {
                SystemError::BadFileDescriptor
            }
            crate::syscall::interface::SyscallError::PermissionDenied => {
                SystemError::PermissionDenied
            }
            crate::syscall::interface::SyscallError::FileNotFound => SystemError::FileNotFound,
            crate::syscall::interface::SyscallError::NoSuchProcess => SystemError::NoSuchProcess,
            crate::syscall::interface::SyscallError::InvalidArgument => {
                SystemError::InvalidArgument
            }
            crate::syscall::interface::SyscallError::OutOfMemory => SystemError::SyscallOutOfMemory,
        }
    }
}

impl From<crate::fs::FsError> for SystemError {
    fn from(error: crate::fs::FsError) -> Self {
        match error {
            crate::fs::FsError::FileNotFound => SystemError::FileNotFound,
            crate::fs::FsError::FileExists => SystemError::FileExists,
            crate::fs::FsError::PermissionDenied => SystemError::PermissionDenied,
            crate::fs::FsError::InvalidFileDescriptor => SystemError::FsInvalidFileDescriptor,
            crate::fs::FsError::InvalidSeek => SystemError::InvalidSeek,
            crate::fs::FsError::DiskFull => SystemError::DiskFull,
        }
    }
}

impl From<crate::memory_management::MapError> for SystemError {
    fn from(error: crate::memory_management::MapError) -> Self {
        match error {
            crate::memory_management::MapError::MappingFailed => SystemError::MappingFailed,
            crate::memory_management::MapError::UnmappingFailed => SystemError::UnmappingFailed,
            crate::memory_management::MapError::FrameAllocationFailed => {
                SystemError::FrameAllocationFailed
            }
        }
    }
}

impl From<crate::memory_management::AllocError> for SystemError {
    fn from(error: crate::memory_management::AllocError) -> Self {
        match error {
            crate::memory_management::AllocError::OutOfMemory => SystemError::MemOutOfMemory,
            crate::memory_management::AllocError::MappingFailed => SystemError::MappingFailed,
        }
    }
}

impl From<crate::loader::LoadError> for SystemError {
    fn from(error: crate::loader::LoadError) -> Self {
        match error {
            crate::loader::LoadError::InvalidFormat => SystemError::InvalidFormat,
            // Map LoadFailed to LoadFailed error code
            _ => SystemError::LoadFailed,
        }
    }
}
