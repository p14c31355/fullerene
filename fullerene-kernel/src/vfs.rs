//! VFS (Virtual File System) stub layer.
//!
//! Provides a unified interface for filesystem operations, delegating
//! to concrete filesystem drivers (tmpfs, FAT32) via mount-point routing.
//! Full implementation requires dentry/inode caching, VFS ops dispatch,
//! and path-resolution logic.

use alloc::string::String;
use alloc::vec::Vec;

/// File descriptor table entry (stub).
#[derive(Debug, Clone)]
pub struct FileDescriptor {
    pub fd: u32,
    pub path: String,
    pub flags: u32,
}

/// In-memory file node (stub).
#[derive(Debug, Clone)]
pub struct VNode {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

/// Placeholder: mount a filesystem at the given mount point.
pub fn mount(_device: &str, _mount_point: &str, _fs_type: &str) -> Result<(), &'static str> {
    log::info!("VFS: mount stub");
    Err("VFS: mount not implemented")
}

/// Placeholder: open a file and return a file descriptor.
pub fn open(_path: &str, _flags: u32) -> Result<FileDescriptor, &'static str> {
    log::info!("VFS: open stub");
    Err("VFS: open not implemented")
}

/// Placeholder: read from a file descriptor.
pub fn read(_fd: u32, _buf: &mut [u8]) -> Result<usize, &'static str> {
    Err("VFS: read not implemented")
}

/// Placeholder: write to a file descriptor.
pub fn write(_fd: u32, _data: &[u8]) -> Result<usize, &'static str> {
    Err("VFS: write not implemented")
}

/// Placeholder: close a file descriptor.
pub fn close(_fd: u32) -> Result<(), &'static str> {
    Err("VFS: close not implemented")
}

/// Placeholder: list directory contents.
pub fn readdir(_path: &str) -> Result<Vec<VNode>, &'static str> {
    log::info!("VFS: readdir stub");
    Err("VFS: readdir not implemented")
}

/// Placeholder: initialise VFS subsystem.
pub fn init() {
    log::info!("VFS: stub initialised (no mounts)");
}