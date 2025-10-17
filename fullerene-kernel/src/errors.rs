//! System error types and conversions - DEPRECATED
//!
//! This module is deprecated. Use `petroleum::common::logging::SystemError` instead.
//! This file is kept for backward compatibility during migration.

// Re-export from petroleum for backward compatibility
pub use petroleum::common::logging::{SystemError, SystemResult};

// Explicit From implementations
petroleum::error_chain!(crate::syscall::interface::SyscallError, SystemError,
    crate::syscall::interface::SyscallError::InvalidSyscall => SystemError::InvalidSyscall,
    crate::syscall::interface::SyscallError::BadFileDescriptor => SystemError::BadFileDescriptor,
    crate::syscall::interface::SyscallError::PermissionDenied => SystemError::PermissionDenied,
    crate::syscall::interface::SyscallError::FileNotFound => SystemError::FileNotFound,
    crate::syscall::interface::SyscallError::NoSuchProcess => SystemError::NoSuchProcess,
    crate::syscall::interface::SyscallError::InvalidArgument => SystemError::InvalidArgument,
    crate::syscall::interface::SyscallError::OutOfMemory => SystemError::SyscallOutOfMemory,
);

petroleum::error_chain!(crate::fs::FsError, SystemError,
    crate::fs::FsError::FileNotFound => SystemError::FileNotFound,
    crate::fs::FsError::FileExists => SystemError::FileExists,
    crate::fs::FsError::PermissionDenied => SystemError::PermissionDenied,
    crate::fs::FsError::InvalidFileDescriptor => SystemError::BadFileDescriptor,
    crate::fs::FsError::InvalidSeek => SystemError::InvalidSeek,
    crate::fs::FsError::DiskFull => SystemError::DiskFull,
);

petroleum::error_chain!(crate::memory_management::MapError, SystemError,
    crate::memory_management::MapError::MappingFailed => SystemError::MappingFailed,
    crate::memory_management::MapError::UnmappingFailed => SystemError::UnmappingFailed,
    crate::memory_management::MapError::FrameAllocationFailed => SystemError::FrameAllocationFailed,
);

petroleum::error_chain!(crate::memory_management::AllocError, SystemError,
    crate::memory_management::AllocError::OutOfMemory => SystemError::MemOutOfMemory,
    crate::memory_management::AllocError::MappingFailed => SystemError::MappingFailed,
);

impl From<crate::loader::LoadError> for SystemError {
    fn from(error: crate::loader::LoadError) -> Self {
        match error {
            crate::loader::LoadError::InvalidFormat => SystemError::InvalidFormat,
            crate::loader::LoadError::OutOfMemory => SystemError::MemOutOfMemory,
            crate::loader::LoadError::AddressAlreadyMapped => SystemError::MappingFailed,
            crate::loader::LoadError::MappingFailed => SystemError::MappingFailed,
            crate::loader::LoadError::NotExecutable
            | crate::loader::LoadError::UnsupportedArchitecture => SystemError::LoadFailed,
        }
    }
}
