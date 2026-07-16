use core::fmt;

use crate::common::logging::SystemError;

/// Memory-management failures shared by allocators and page-table code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryError {
    /// The allocator cannot satisfy the requested capacity.
    OutOfMemory,
    /// A physical frame allocation failed.
    FrameAllocationFailed,
    /// A virtual-memory mapping could not be installed.
    MappingFailed,
    /// A virtual-memory mapping could not be removed.
    UnmappingFailed,
    /// An address is outside the supported virtual or physical range.
    InvalidAddress,
    /// An address or size does not meet the required alignment.
    InvalidAlignment,
    /// A zero or otherwise unsupported region size was requested.
    InvalidSize,
    /// Address arithmetic overflowed.
    AddressOverflow,
    /// The requested virtual range already contains a mapping.
    AlreadyMapped,
    /// The requested virtual range has no mapping.
    NotMapped,
    /// The requested access violates mapping permissions.
    PermissionDenied,
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OutOfMemory => "out of memory",
            Self::FrameAllocationFailed => "frame allocation failed",
            Self::MappingFailed => "memory mapping failed",
            Self::UnmappingFailed => "memory unmapping failed",
            Self::InvalidAddress => "invalid memory address",
            Self::InvalidAlignment => "invalid memory alignment",
            Self::InvalidSize => "invalid memory size",
            Self::AddressOverflow => "memory address overflow",
            Self::AlreadyMapped => "memory range already mapped",
            Self::NotMapped => "memory range is not mapped",
            Self::PermissionDenied => "memory access denied",
        })
    }
}

impl From<MemoryError> for SystemError {
    fn from(error: MemoryError) -> Self {
        match error {
            MemoryError::OutOfMemory => Self::MemOutOfMemory,
            MemoryError::FrameAllocationFailed => Self::FrameAllocationFailed,
            MemoryError::MappingFailed | MemoryError::AlreadyMapped => Self::MappingFailed,
            MemoryError::UnmappingFailed | MemoryError::NotMapped => Self::UnmappingFailed,
            MemoryError::PermissionDenied => Self::PermissionDenied,
            MemoryError::InvalidAddress
            | MemoryError::InvalidAlignment
            | MemoryError::InvalidSize
            | MemoryError::AddressOverflow => Self::InvalidArgument,
        }
    }
}

impl TryFrom<SystemError> for MemoryError {
    type Error = SystemError;

    fn try_from(error: SystemError) -> Result<Self, Self::Error> {
        match error {
            SystemError::MemOutOfMemory | SystemError::SyscallOutOfMemory => Ok(Self::OutOfMemory),
            SystemError::FrameAllocationFailed => Ok(Self::FrameAllocationFailed),
            SystemError::MappingFailed => Ok(Self::MappingFailed),
            SystemError::UnmappingFailed => Ok(Self::UnmappingFailed),
            SystemError::PermissionDenied => Ok(Self::PermissionDenied),
            other => Err(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_memory_errors_convert_without_string_matching() {
        assert_eq!(
            MemoryError::try_from(SystemError::FrameAllocationFailed),
            Ok(MemoryError::FrameAllocationFailed)
        );
        assert_eq!(
            MemoryError::try_from(SystemError::FileNotFound),
            Err(SystemError::FileNotFound)
        );
    }

    #[test]
    fn typed_memory_errors_remain_compatible_with_system_error() {
        assert_eq!(
            SystemError::from(MemoryError::AddressOverflow),
            SystemError::InvalidArgument
        );
        assert_eq!(
            SystemError::from(MemoryError::MappingFailed),
            SystemError::MappingFailed
        );
    }
}
