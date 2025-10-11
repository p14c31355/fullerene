//! Basic filesystem implementation for Fullerene OS
//!
//! Currently implements a simple RAM-based filesystem for basic file operations.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

/// File descriptor type
pub type FileDescriptor = i32;

/// File permissions
#[derive(Debug, Clone, Copy)]
pub struct FilePermissions {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

/// File structure
pub struct File {
    pub name: String,
    pub data: Vec<u8>,
    pub permissions: FilePermissions,
    pub position: usize,
}

/// Global filesystem state
static FILESYSTEM: Mutex<BTreeMap<String, File>> = Mutex::new(BTreeMap::new());
static NEXT_FD: Mutex<FileDescriptor> = Mutex::new(3); // 0,1,2 are reserved for stdio
static OPEN_FILES: Mutex<BTreeMap<FileDescriptor, String>> = Mutex::new(BTreeMap::new());

/// Initialize filesystem
pub fn init() {
    // Create some basic files if needed
    // For now, start with empty filesystem, which is good for test isolation.
    FILESYSTEM.lock().clear();
    *NEXT_FD.lock() = 3;
    OPEN_FILES.lock().clear();
}

/// Create a new file
pub fn create_file(name: &str, data: &[u8]) -> Result<(), FsError> {
    let mut fs = FILESYSTEM.lock();
    if fs.contains_key(name) {
        return Err(FsError::FileExists);
    }

    let file = File {
        name: String::from(name),
        data: data.to_vec(),
        permissions: FilePermissions {
            read: true,
            write: true,
            execute: false,
        },
        position: 0,
    };

    fs.insert(String::from(name), file);
    Ok(())
}

/// Open a file and return file descriptor
pub fn open_file(name: &str) -> Result<FileDescriptor, FsError> {
    // Acquire locks in consistent order: FILESYSTEM then OPEN_FILES then NEXT_FD
    let fs = FILESYSTEM.lock();
    if !fs.contains_key(name) {
        return Err(FsError::FileNotFound);
    }

    // While still holding FILESYSTEM lock, acquire OPEN_FILES and NEXT_FD
    let mut open_files = OPEN_FILES.lock();
    let mut next_fd = NEXT_FD.lock();

    let fd = *next_fd;
    *next_fd += 1;

    open_files.insert(fd, String::from(name));

    // Explicitly drop locks in reverse order (optional but good practice)
    drop(next_fd);
    drop(open_files);
    drop(fs);

    Ok(fd)
}

/// Close a file
pub fn close_file(fd: FileDescriptor) -> Result<(), FsError> {
    let mut open_files = OPEN_FILES.lock();
    if !open_files.contains_key(&fd) {
        return Err(FsError::InvalidFileDescriptor);
    }
    open_files.remove(&fd);
    Ok(())
}

/// Read from file
pub fn read_file(fd: FileDescriptor, buffer: &mut [u8]) -> Result<usize, FsError> {
    let mut fs = FILESYSTEM.lock();
    let open_files = OPEN_FILES.lock();

    let filename = open_files.get(&fd).ok_or(FsError::InvalidFileDescriptor)?;
    let file = fs.get_mut(filename).ok_or(FsError::FileNotFound)?;

    if !file.permissions.read {
        return Err(FsError::PermissionDenied);
    }

    let remaining = file.data.len().saturating_sub(file.position);
    let to_read = remaining.min(buffer.len());

    buffer[..to_read].copy_from_slice(&file.data[file.position..file.position + to_read]);
    file.position += to_read;

    Ok(to_read)
}

/// Write to file
pub fn write_file(fd: FileDescriptor, data: &[u8]) -> Result<usize, FsError> {
    let mut fs = FILESYSTEM.lock();
    let open_files = OPEN_FILES.lock();

    let filename = open_files.get(&fd).ok_or(FsError::InvalidFileDescriptor)?;
    let file = fs.get_mut(filename).ok_or(FsError::FileNotFound)?;

    if !file.permissions.write {
        return Err(FsError::PermissionDenied);
    }

    // Simple append for now
    file.data.extend_from_slice(data);
    Ok(data.len())
}

/// Seek in file
pub fn seek_file(fd: FileDescriptor, position: usize) -> Result<(), FsError> {
    let open_files = OPEN_FILES.lock();
    let filename = open_files.get(&fd).ok_or(FsError::InvalidFileDescriptor)?;
    let filename = filename.clone(); // Clone the filename for use after lock is released
    drop(open_files); // Release the lock before acquiring FILESYSTEM lock

    let mut fs = FILESYSTEM.lock();
    let file = fs.get_mut(&filename).ok_or(FsError::FileNotFound)?;

    if position > file.data.len() {
        return Err(FsError::InvalidSeek);
    }

    file.position = position;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_open_file() {
        init();
        create_file("test.txt", b"Hello World").unwrap();
        let fd = open_file("test.txt").unwrap();
        assert!(fd >= 3); // Should be >= 3

        close_file(fd).unwrap();
    }

    #[test]
    fn test_read_write_file() {
        init();
        create_file("test.txt", b"Hello").unwrap();
        let fd = open_file("test.txt").unwrap();

        let mut buffer = [0u8; 10];
        let read_bytes = read_file(fd, &mut buffer).unwrap();
        assert_eq!(read_bytes, 5);
        assert_eq!(&buffer[..5], b"Hello");

        close_file(fd).unwrap();
    }
}
