use alloc::string::String;
use alloc::vec::Vec;

use crate::vfs;
pub use genome::fs::{DirEntry, FsError, PackageEntry, parse_manifest};

pub fn init() {
    vfs::init();
    log::info!("File system initialized (VFS + tmpfs)");
}

// ── File descriptor wrapper ───────────────────────────────────

#[derive(Debug, Clone)]
pub struct FileDesc {
    pub fd: u32,
    pub ino: u64,
    pub offset: usize,
    pub flags: u32,
}

impl From<genome::vfs::FileDescriptor> for FileDesc {
    fn from(v: genome::vfs::FileDescriptor) -> Self {
        Self {
            fd: v.fd,
            ino: v.ino,
            offset: v.offset,
            flags: v.flags,
        }
    }
}

// ── Public file operations ────────────────────────────────────

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

pub fn create_dir(path: &str) -> Result<(), FsError> {
    vfs::mkdir(path).map_err(|e| map_vfs_error(e))
}

pub fn remove(path: &str) -> Result<(), FsError> {
    vfs::unlink(path).map_err(|e| map_vfs_error(e))
}

pub fn open_file(path: &str) -> Result<FileDesc, FsError> {
    vfs::open(path, 0)
        .map(FileDesc::from)
        .map_err(|e| map_vfs_error(e))
}

pub fn close_file(fd: FileDesc) -> Result<(), FsError> {
    vfs::close(fd.fd).map_err(|e| map_vfs_error(e))
}

pub fn read_file(fd: &mut FileDesc, buffer: &mut [u8]) -> Result<usize, FsError> {
    let n = vfs::read(fd.fd, buffer).map_err(|e| map_vfs_error(e))?;
    fd.offset += n;
    Ok(n)
}

pub fn write_file(fd: &mut FileDesc, data: &[u8]) -> Result<usize, FsError> {
    vfs::write(fd.fd, data).map_err(|e| map_vfs_error(e))
}

pub fn seek_file(fd: &mut FileDesc, position: usize) -> Result<(), FsError> {
    vfs::seek(fd.fd, position)
        .map(|_| {
            fd.offset = position;
        })
        .map_err(|e| map_vfs_error(e))
}

pub fn list_dir(path: &str) -> Result<Vec<DirEntry>, FsError> {
    vfs::readdir(path)
        .map(|entries| {
            entries
                .into_iter()
                .map(|v| DirEntry {
                    name: v.name,
                    size: v.size,
                    is_dir: v.is_dir,
                })
                .collect()
        })
        .map_err(|e| map_vfs_error(e))
}

pub fn exists(path: &str) -> bool {
    match vfs::open(path, 0) {
        Ok(fd_info) => {
            let _ = vfs::close(fd_info.fd);
            true
        }
        Err(_) => false,
    }
}

pub fn mount(device: &str, mount_point: &str, fs_type: &str) -> Result<(), FsError> {
    vfs::mount(device, mount_point, fs_type).map_err(|e| map_vfs_error(e))
}

pub fn working_directory() -> Result<String, FsError> {
    vfs::working_directory().map_err(|e| map_vfs_error(e))
}

pub fn change_directory(path: &str) -> Result<(), FsError> {
    vfs::change_directory(path).map_err(|e| map_vfs_error(e))
}

pub fn copy_file(src: &str, dst: &str) -> Result<(), FsError> {
    let data = read_entire_file(src)?;
    write_entire_file(dst, &data)
}

pub fn move_file(src: &str, dst: &str) -> Result<(), FsError> {
    copy_file(src, dst)?;
    remove(src)
}

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

pub fn write_entire_file(path: &str, data: &[u8]) -> Result<(), FsError> {
    if exists(path) {
        let _ = remove(path);
    }
    create_file(path, data)
}

pub fn file_size(path: &str) -> Result<u64, FsError> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return Ok(0);
    }
    let (parent_path, filename) = if let Some(pos) = trimmed.rfind('/') {
        if pos == 0 {
            ("/", &trimmed[1..])
        } else {
            (&trimmed[..pos], &trimmed[pos + 1..])
        }
    } else {
        ("/", trimmed)
    };

    let entries = list_dir(parent_path)?;
    entries
        .iter()
        .find(|e| e.name == filename)
        .map(|e| e.size)
        .ok_or(FsError::FileNotFound)
}

// ── Package management ─────────────────────────────────────

pub fn list_packages() -> Result<Vec<PackageEntry>, FsError> {
    let mut packages = Vec::new();
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

pub fn remove_package(name: &str) -> Result<(), FsError> {
    let pkg_dir = alloc::format!("/packages/{}", name);
    if !exists(&pkg_dir) {
        return Err(FsError::FileNotFound);
    }
    let entries = walk_dir(&pkg_dir)?;
    let mut sorted_entries = entries;
    sorted_entries.sort_by(|a, b| b.len().cmp(&a.len()));

    for entry in sorted_entries {
        remove(&entry)?;
    }

    remove(&pkg_dir)
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
