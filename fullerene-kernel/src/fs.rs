//! File system integration module for Fullerene OS
//!
//! This module bridges the kernel's file operations to the VFS (Virtual File System)
//! backed by tmpfs. All shell commands (ls, cat, etc.) use these APIs.
//!
//! # Architecture
//!
//! ```text
//! Shell commands → fs.rs → vfs.rs → tmpfs (BTree in-memory)
//!                 fs.rs → vfs.rs → FAT32 (future)
//! ```

use crate::vfs;
use alloc::string::String;
use alloc::vec::Vec;

/// Initialize the file system by mounting the VFS.
pub fn init() {
    vfs::init();
    log::info!("File system initialized (VFS + tmpfs)");
}

// ── File system errors ────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    FileNotFound,
    FileExists,
    PermissionDenied,
    InvalidFileDescriptor,
    InvalidSeek,
    DiskFull,
    NotADirectory,
    DirectoryNotEmpty,
    IsADirectory,
    InvalidPath,
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
            FsError::NotADirectory
            | FsError::DirectoryNotEmpty
            | FsError::IsADirectory
            | FsError::InvalidPath => petroleum::common::logging::SystemError::InvalidArgument,
        }
    }
}

impl core::fmt::Display for FsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FsError::FileNotFound => write!(f, "file not found"),
            FsError::FileExists => write!(f, "file already exists"),
            FsError::PermissionDenied => write!(f, "permission denied"),
            FsError::InvalidFileDescriptor => write!(f, "invalid file descriptor"),
            FsError::InvalidSeek => write!(f, "invalid seek"),
            FsError::DiskFull => write!(f, "disk full"),
            FsError::NotADirectory => write!(f, "not a directory"),
            FsError::DirectoryNotEmpty => write!(f, "directory not empty"),
            FsError::IsADirectory => write!(f, "is a directory"),
            FsError::InvalidPath => write!(f, "invalid path"),
        }
    }
}

// ── File descriptor ───────────────────────────────────────────

/// File descriptor wrapper for kernel operations.
#[derive(Debug, Clone)]
pub struct FileDesc {
    pub fd: u32,
    pub ino: u64,
    pub offset: usize,
    pub flags: u32,
}

impl From<vfs::FileDescriptor> for FileDesc {
    fn from(v: vfs::FileDescriptor) -> Self {
        Self {
            fd: v.fd,
            ino: v.ino,
            offset: v.offset,
            flags: v.flags,
        }
    }
}

// ── VNode wrapper ─────────────────────────────────────────────

/// File/directory metadata visible to userspace.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

impl From<vfs::VNode> for DirEntry {
    fn from(v: vfs::VNode) -> Self {
        Self {
            name: v.name,
            size: v.size,
            is_dir: v.is_dir,
        }
    }
}

// ── Public file operations ────────────────────────────────────

/// Create a new file at the given path and write initial data.
pub fn create_file(path: &str, data: &[u8]) -> Result<(), FsError> {
    let fd_info = vfs::create(path).map_err(|e| map_vfs_error(e))?;
    if !data.is_empty() {
        vfs::write(fd_info.fd, data).map_err(|e| {
            let _ = vfs::close(fd_info.fd);
            map_vfs_error(e)
        })?;
    }
    let _ = vfs::close(fd_info.fd);
    Ok(())
}

/// Create a directory.
pub fn create_dir(path: &str) -> Result<(), FsError> {
    vfs::mkdir(path).map_err(|e| map_vfs_error(e))
}

/// Remove a file or empty directory.
pub fn remove(path: &str) -> Result<(), FsError> {
    vfs::unlink(path).map_err(|e| map_vfs_error(e))
}

/// Open a file and return a file descriptor.
pub fn open_file(path: &str) -> Result<FileDesc, FsError> {
    vfs::open(path, 0)
        .map(FileDesc::from)
        .map_err(|e| map_vfs_error(e))
}

/// Close a file descriptor.
pub fn close_file(fd: FileDesc) -> Result<(), FsError> {
    vfs::close(fd.fd).map_err(|e| map_vfs_error(e))
}

/// Read from a file descriptor into a buffer.
/// Returns the number of bytes read.
pub fn read_file(fd: &mut FileDesc, buffer: &mut [u8]) -> Result<usize, FsError> {
    let n = vfs::read(fd.fd, buffer).map_err(|e| map_vfs_error(e))?;
    fd.offset += n;
    Ok(n)
}

/// Write data to a file descriptor.
/// Returns the number of bytes written.
pub fn write_file(fd: &mut FileDesc, data: &[u8]) -> Result<usize, FsError> {
    vfs::write(fd.fd, data).map_err(|e| map_vfs_error(e))
}

/// Seek to a position in a file.
pub fn seek_file(fd: &mut FileDesc, position: usize) -> Result<(), FsError> {
    vfs::seek(fd.fd, position)
        .map(|_| {
            fd.offset = position;
        })
        .map_err(|e| map_vfs_error(e))
}

/// List directory contents.
pub fn list_dir(path: &str) -> Result<Vec<DirEntry>, FsError> {
    vfs::readdir(path)
        .map(|entries| entries.into_iter().map(DirEntry::from).collect())
        .map_err(|e| map_vfs_error(e))
}

/// Check if a path exists.
pub fn exists(path: &str) -> bool {
    match vfs::open(path, 0) {
        Ok(fd_info) => {
            let _ = vfs::close(fd_info.fd);
            true
        }
        Err(_) => false,
    }
}

/// Mount a filesystem (currently only tmpfs is supported).
pub fn mount(device: &str, mount_point: &str, fs_type: &str) -> Result<(), FsError> {
    vfs::mount(device, mount_point, fs_type).map_err(|e| map_vfs_error(e))
}

/// Get the current working directory.
pub fn working_directory() -> Result<String, FsError> {
    vfs::working_directory().map_err(|e| map_vfs_error(e))
}

/// Change the current working directory.
pub fn change_directory(path: &str) -> Result<(), FsError> {
    vfs::change_directory(path).map_err(|e| map_vfs_error(e))
}

/// Copy a file from `src` to `dst`.
pub fn copy_file(src: &str, dst: &str) -> Result<(), FsError> {
    let data = read_entire_file(src)?;
    write_entire_file(dst, &data)
}

/// Move a file from `src` to `dst`.
pub fn move_file(src: &str, dst: &str) -> Result<(), FsError> {
    copy_file(src, dst)?;
    remove(src)
}

/// Recursively collect all entries under a directory (for `tree`/`find`).
pub fn walk_dir(path: &str) -> Result<Vec<String>, FsError> {
    let mut result = Vec::new();
    let entries = list_dir(path)?;
    for entry in &entries {
        let full = if path.ends_with('/') {
            alloc::format!("{}{}", path, entry.name)
        } else {
            alloc::format!("{}/{}", path, entry.name)
        };
        result.push(full.clone());
        if entry.is_dir {
            let children = walk_dir(&full)?;
            result.extend(children);
        }
    }
    Ok(result)
}

// ── Convenience wrappers for shell commands ───────────────────

/// Read entire file contents as bytes.
pub fn read_entire_file(path: &str) -> Result<Vec<u8>, FsError> {
    let mut fd = open_file(path)?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    let result = loop {
        match read_file(&mut fd, &mut chunk) {
            Ok(n) => {
                if n == 0 {
                    break Ok(buf);
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Err(e) => {
                break Err(e);
            }
        }
    };
    let _ = close_file(fd);
    result
}

/// Write an entire file from bytes.
pub fn write_entire_file(path: &str, data: &[u8]) -> Result<(), FsError> {
    // Delete existing file first
    if exists(path) {
        let _ = remove(path);
    }
    create_file(path, data)
}

/// Get file size by querying filesystem metadata.
pub fn file_size(path: &str) -> Result<u64, FsError> {
    // Split path into parent directory and filename
    let (parent_path, filename) = if let Some(pos) = path.rfind('/') {
        if pos == 0 {
            ("/", &path[1..])
        } else {
            (&path[..pos], &path[pos + 1..])
        }
    } else {
        ("/", path)
    };

    // List parent directory to find the file entry
    let entries = list_dir(parent_path)?;
    entries
        .iter()
        .find(|e| e.name == filename)
        .map(|e| e.size)
        .ok_or(FsError::FileNotFound)
}

// ── Package management ─────────────────────────────────────

/// A package entry parsed from /packages/<name>/manifest.txt.
#[derive(Debug, Clone)]
pub struct PackageEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub binary: String,
}

/// List all installed packages under /packages/.
pub fn list_packages() -> Result<Vec<PackageEntry>, FsError> {
    let mut packages = Vec::new();
    // Ensure /packages directory exists
    if !exists("/packages") {
        create_dir("/packages")?;
        return Ok(packages);
    }
    let entries = list_dir("/packages")?;
    for entry in &entries {
        if entry.is_dir {
            let manifest_path = alloc::format!("/packages/{}/manifest.txt", entry.name);
            if exists(&manifest_path) {
                if let Ok(data) = read_entire_file(&manifest_path) {
                    if let Ok(text) = core::str::from_utf8(&data) {
                        if let Some(pkg) = parse_manifest(&entry.name, text) {
                            packages.push(pkg);
                        }
                    }
                }
            }
        }
    }
    Ok(packages)
}

/// Install a package: create directory, write manifest and binary.
pub fn install_package(
    name: &str,
    version: &str,
    description: &str,
    binary: &[u8],
) -> Result<(), FsError> {
    let pkg_dir = alloc::format!("/packages/{}", name);
    if exists(&pkg_dir) {
        return Err(FsError::FileExists);
    }
    create_dir(&pkg_dir)?;

    let manifest = alloc::format!(
        "name = \"{}\"\nversion = \"{}\"\ndescription = \"{}\"\nbinary = \"app.bin\"\n",
        name,
        version,
        description
    );
    let manifest_path = alloc::format!("/packages/{}/manifest.txt", name);
    write_entire_file(&manifest_path, manifest.as_bytes())?;

    let bin_path = alloc::format!("/packages/{}/app.bin", name);
    write_entire_file(&bin_path, binary)?;

    Ok(())
}

/// Remove a package (directory and all contents).
pub fn remove_package(name: &str) -> Result<(), FsError> {
    let pkg_dir = alloc::format!("/packages/{}", name);
    if !exists(&pkg_dir) {
        return Err(FsError::FileNotFound);
    }
    // Use walk_dir to recursively delete all contents
    let entries = walk_dir(&pkg_dir)?;
    // Sort in reverse order to delete files before their parent directories
    let mut sorted_entries = entries;
    sorted_entries.sort_by(|a, b| b.len().cmp(&a.len()));

    // Remove all files and subdirectories
    // Remove all files and subdirectories
    for entry in sorted_entries {
        remove(&entry)?;
    }

    // Finally remove the package directory itself
    remove(&pkg_dir)
}

/// Parse a manifest.txt into a PackageEntry.
fn parse_manifest(name: &str, text: &str) -> Option<PackageEntry> {
    let mut version = String::from("0.1.0");
    let mut description = String::new();
    let mut binary = String::from("app.bin");

    for line in text.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            match key {
                "version" => version = String::from(value),
                "description" => description = String::from(value),
                "binary" => binary = String::from(value),
                _ => {}
            }
        }
    }

    Some(PackageEntry {
        name: String::from(name),
        version,
        description,
        binary,
    })
}

// ── Error mapping ─────────────────────────────────────────────

fn map_vfs_error(e: &str) -> FsError {
    match e {
        "not found" => FsError::FileNotFound,
        "bad fd" => FsError::InvalidFileDescriptor,
        "inode not found" => FsError::FileNotFound,
        "not a file" => FsError::IsADirectory,
        "directory not empty" => FsError::DirectoryNotEmpty,
        "only tmpfs is supported" => FsError::PermissionDenied,
        "vfs not init" => FsError::PermissionDenied,
        "create failed" => FsError::FileExists,
        "open failed after create" => FsError::FileExists,
        "invalid path" => FsError::InvalidPath,
        "mkdir failed" => FsError::PermissionDenied,
        _ => FsError::InvalidFileDescriptor,
    }
}
