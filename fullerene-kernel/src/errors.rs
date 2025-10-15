//! System error types and conversions - DEPRECATED
//!
//! This module is deprecated. Use `petroleum::common::logging::SystemError` instead.
//! This file is kept for backward compatibility during migration.

// Re-export from petroleum for backward compatibility
pub use petroleum::common::logging::{SystemError, SystemResult};

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
            crate::fs::FsError::InvalidFileDescriptor => SystemError::BadFileDescriptor,
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
            crate::loader::LoadError::OutOfMemory => SystemError::MemOutOfMemory,
            crate::loader::LoadError::AddressAlreadyMapped => SystemError::MappingFailed,
            crate::loader::LoadError::MappingFailed => SystemError::MappingFailed,
            _ => SystemError::LoadFailed,
        }
    }
}
