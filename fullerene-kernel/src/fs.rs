//! Basic filesystem stub for Fullerene OS
//!
//! Currently a minimal placeholder to allow boot to proceed to desktop.

/// Initialize filesystem (stub - no-op for boot)
pub fn init() {
    // No initialization needed for boot. Filesystem operations will be
    // added once the kernel reaches a stable desktop state.
}

/// File system errors
#[derive(Debug, Clone, Copy)]
pub enum FsError {
    FileNotFound,
    FileExists,
    PermissionDenied,
    InvalidFileDescriptor,
    InvalidSeek,
    DiskFull,
}

impl From<FsError> for petroleum::common::logging::SystemError {
    fn from(error: FsError) -> Self {
        match error {
            FsError::FileNotFound => petroleum::common::logging::SystemError::FileNotFound,
            FsError::FileExists => petroleum::common::logging::SystemError::FileExists,
            FsError::PermissionDenied => petroleum::common::logging::SystemError::PermissionDenied,
            FsError::InvalidFileDescriptor => {
                petroleum::common::logging::SystemError::BadFileDescriptor
            }
            FsError::InvalidSeek => petroleum::common::logging::SystemError::InvalidSeek,
            FsError::DiskFull => petroleum::common::logging::SystemError::DiskFull,
        }
    }
}

/// Stub functions that return errors gracefully
pub fn create_file(_name: &str, _data: &[u8]) -> Result<(), FsError> {
    Err(FsError::DiskFull)
}

pub fn open_file(_name: &str) -> Result<i32, FsError> {
    Err(FsError::FileNotFound)
}

pub fn close_file(_fd: i32) -> Result<(), FsError> {
    Err(FsError::InvalidFileDescriptor)
}

pub fn read_file(_fd: i32, _buffer: &mut [u8]) -> Result<usize, FsError> {
    Err(FsError::InvalidFileDescriptor)
}

pub fn write_file(_fd: i32, _data: &[u8]) -> Result<usize, FsError> {
    Err(FsError::InvalidFileDescriptor)
}

pub fn seek_file(_fd: i32, _position: usize) -> Result<(), FsError> {
    Err(FsError::InvalidFileDescriptor)
}
