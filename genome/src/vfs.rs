use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::fs::FsError;
use crate::io::{FileReader, Read, Seek, SeekFrom};

const MAX_SYMLINK_DEPTH: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InodeType {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone)]
struct Inode {
    name: String,
    kind: InodeType,
    data: Vec<u8>,
    children: Vec<u64>,
    parent: u64,
    target: Option<String>,
    size: u64,
}

impl Inode {
    fn new(name: &str, kind: InodeType, parent: u64) -> Self {
        Self {
            name: String::from(name),
            kind,
            data: Vec::new(),
            children: Vec::new(),
            parent,
            target: None,
            size: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileDescriptor {
    pub fd: u32,
    pub ino: u64,
    pub offset: u64,
    pub flags: u32,
}

#[derive(Debug, Clone)]
pub struct VNode {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

/// Operations and limits a mounted filesystem promises to support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSystemCapabilities {
    pub read_only: bool,
    pub mkdir: bool,
    pub unlink: bool,
    pub symlink: bool,
    pub large_file: bool,
}

impl FileSystemCapabilities {
    pub const fn new(
        read_only: bool,
        mkdir: bool,
        unlink: bool,
        symlink: bool,
        large_file: bool,
    ) -> Self {
        Self {
            read_only,
            mkdir,
            unlink,
            symlink,
            large_file,
        }
    }

    pub const fn conservative() -> Self {
        Self::new(true, false, false, false, false)
    }
}

pub trait FileSystem: Send {
    fn capabilities(&self) -> FileSystemCapabilities {
        FileSystemCapabilities::conservative()
    }
    fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor>;
    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, FsError>;
    fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, FsError>;
    fn close(&mut self, fd: u32) -> Result<(), FsError>;
    fn seek(&mut self, fd: u32, pos: u64) -> Result<(), FsError>;
    fn position(&mut self, _fd: u32) -> Result<u64, FsError> {
        Err(FsError::NotSupported)
    }
    fn size(&mut self, _fd: u32) -> Result<u64, FsError> {
        Err(FsError::NotSupported)
    }
    fn create(&mut self, path: &str, kind: InodeType) -> Option<u64>;
    fn mkdir(&mut self, path: &str) -> Result<(), FsError>;
    fn unlink(&mut self, path: &str) -> Result<(), FsError>;
    fn readdir(&mut self, path: &str) -> Result<Vec<VNode>, FsError>;
    fn exists(&mut self, path: &str) -> bool;
}

// ── MemFileSystem ─────────────────────────────────────────────

pub struct MemFileSystem {
    inodes: BTreeMap<u64, Inode>,
    next_ino: u64,
    fds: BTreeMap<u32, FileDescriptor>,
    next_fd: u32,
}

impl MemFileSystem {
    pub fn new() -> Self {
        let root = Inode::new("", InodeType::Directory, 0);
        let mut inodes = BTreeMap::new();
        inodes.insert(1, root);
        Self {
            inodes,
            next_ino: 2,
            fds: BTreeMap::new(),
            next_fd: 0,
        }
    }

    fn lookup(&self, path: &str) -> Option<u64> {
        self.lookup_from(path, 1, 0)
    }

    fn lookup_from(&self, path: &str, start_ino: u64, depth: u32) -> Option<u64> {
        if depth > MAX_SYMLINK_DEPTH || path.is_empty() {
            return (depth <= MAX_SYMLINK_DEPTH).then_some(start_ino);
        }
        let (effective_start, trimmed) = if path.starts_with('/') {
            (1u64, path.trim_start_matches('/'))
        } else {
            (start_ino, path)
        };
        let components: Vec<&str> = trimmed.split('/').filter(|c| !c.is_empty()).collect();
        if components.is_empty() {
            return Some(effective_start);
        }
        let mut current = effective_start;
        for (idx, comp) in components.iter().enumerate() {
            let parent_ino = current;
            match *comp {
                "." => {}
                ".." => {
                    let ino = self.inodes.get(&current)?;
                    current = if ino.parent == 0 { 1 } else { ino.parent };
                }
                _ => current = self.lookup_child(current, comp)?,
            }
            if let Some(ref target) = self.inodes.get(&current)?.target {
                let mut new_path = target.clone();
                for trailing in &components[idx + 1..] {
                    new_path.push('/');
                    new_path.push_str(trailing);
                }
                let resolve_start = if target.starts_with('/') {
                    1
                } else {
                    parent_ino
                };
                return self.lookup_from(&new_path, resolve_start, depth + 1);
            }
        }
        Some(current)
    }

    fn lookup_parent(&self, path: &str) -> Option<(u64, String)> {
        let path = path.trim_end_matches('/');
        if path.is_empty() || path == "/" {
            return None;
        }
        if let Some(last_slash) = path.rfind('/') {
            let parent_path = if last_slash == 0 {
                "/"
            } else {
                &path[..last_slash]
            };
            let name = String::from(&path[last_slash + 1..]);
            let parent_ino = self.lookup(parent_path)?;
            Some((parent_ino, name))
        } else {
            Some((1, String::from(path)))
        }
    }

    fn lookup_child(&self, parent_ino: u64, name: &str) -> Option<u64> {
        let parent = self.inodes.get(&parent_ino)?;
        parent
            .children
            .iter()
            .find(|&&c| self.inodes.get(&c).is_some_and(|i| i.name.as_str() == name))
            .copied()
    }
}

impl FileSystem for MemFileSystem {
    fn capabilities(&self) -> FileSystemCapabilities {
        FileSystemCapabilities::new(false, true, true, true, true)
    }

    fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor> {
        let ino = self.lookup(path)?;
        let fd = self.next_fd;
        self.next_fd += 1;
        let desc = FileDescriptor {
            fd,
            ino,
            offset: 0,
            flags,
        };
        self.fds.insert(fd, desc.clone());
        Some(desc)
    }

    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        let desc = self
            .fds
            .get_mut(&fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
        let ino = self.inodes.get(&desc.ino).ok_or(FsError::FileNotFound)?;
        if ino.kind != InodeType::File {
            return Err(FsError::IsADirectory);
        }
        let offset = usize::try_from(desc.offset).map_err(|_| FsError::InvalidSeek)?;
        if offset >= ino.data.len() {
            return Ok(0);
        }
        let data = &ino.data[offset..];
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        desc.offset = desc
            .offset
            .checked_add(n as u64)
            .ok_or(FsError::InvalidSeek)?;
        Ok(n)
    }

    fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, FsError> {
        let desc = self
            .fds
            .get_mut(&fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
        let ino = self
            .inodes
            .get_mut(&desc.ino)
            .ok_or(FsError::FileNotFound)?;
        if ino.kind != InodeType::File {
            return Err(FsError::IsADirectory);
        }
        let off = usize::try_from(desc.offset).map_err(|_| FsError::InvalidSeek)?;
        let new_len = off.checked_add(data.len()).ok_or(FsError::InvalidInput)?;
        if new_len > ino.data.len() {
            ino.data.resize(new_len, 0);
        }
        ino.data[off..off + data.len()].copy_from_slice(data);
        ino.size = ino.data.len() as u64;
        desc.offset = desc
            .offset
            .checked_add(data.len() as u64)
            .ok_or(FsError::InvalidSeek)?;
        Ok(data.len())
    }

    fn close(&mut self, fd: u32) -> Result<(), FsError> {
        self.fds.remove(&fd).ok_or(FsError::InvalidFileDescriptor)?;
        Ok(())
    }

    fn seek(&mut self, fd: u32, pos: u64) -> Result<(), FsError> {
        let desc = self
            .fds
            .get_mut(&fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
        desc.offset = pos;
        Ok(())
    }

    fn position(&mut self, fd: u32) -> Result<u64, FsError> {
        self.fds
            .get(&fd)
            .map(|descriptor| descriptor.offset)
            .ok_or(FsError::InvalidFileDescriptor)
    }

    fn size(&mut self, fd: u32) -> Result<u64, FsError> {
        let descriptor = self.fds.get(&fd).ok_or(FsError::InvalidFileDescriptor)?;
        self.inodes
            .get(&descriptor.ino)
            .map(|inode| inode.size)
            .ok_or(FsError::FileNotFound)
    }

    fn create(&mut self, path: &str, kind: InodeType) -> Option<u64> {
        if self.lookup(path).is_some() {
            return None;
        }
        let (parent_ino, name) = self.lookup_parent(path)?;
        let parent = self.inodes.get(&parent_ino)?;
        if parent.kind != InodeType::Directory {
            return None;
        }
        let ino = self.next_ino;
        self.next_ino = ino + 1;
        let inode = Inode::new(&name, kind, parent_ino);
        self.inodes.insert(ino, inode);
        if let Some(parent) = self.inodes.get_mut(&parent_ino) {
            parent.children.push(ino);
        }
        Some(ino)
    }

    fn mkdir(&mut self, path: &str) -> Result<(), FsError> {
        if path == "/" {
            return Ok(());
        }
        let (_, _) = self.lookup_parent(path).ok_or(FsError::InvalidPath)?;
        self.create(path, InodeType::Directory)
            .ok_or(FsError::PermissionDenied)?;
        Ok(())
    }

    fn unlink(&mut self, path: &str) -> Result<(), FsError> {
        let (parent_ino, name) = self.lookup_parent(path).ok_or(FsError::FileNotFound)?;
        let child_ino = self
            .lookup_child(parent_ino, &name)
            .ok_or(FsError::FileNotFound)?;
        let child = self.inodes.get(&child_ino).ok_or(FsError::FileNotFound)?;
        if child.kind == InodeType::Directory && !child.children.is_empty() {
            return Err(FsError::DirectoryNotEmpty);
        }
        if let Some(parent) = self.inodes.get_mut(&parent_ino) {
            parent.children.retain(|&c| c != child_ino);
        }
        self.inodes.remove(&child_ino);
        Ok(())
    }

    fn readdir(&mut self, path: &str) -> Result<Vec<VNode>, FsError> {
        let ino = self.lookup(path).ok_or(FsError::FileNotFound)?;
        let inode = self.inodes.get(&ino).ok_or(FsError::FileNotFound)?;
        if inode.kind != InodeType::Directory {
            return Err(FsError::NotADirectory);
        }
        let mut entries = Vec::new();
        for &c in &inode.children {
            if let Some(child) = self.inodes.get(&c) {
                entries.push(VNode {
                    name: child.name.clone(),
                    size: child.size,
                    is_dir: child.kind == InodeType::Directory,
                });
            }
        }
        Ok(entries)
    }

    fn exists(&mut self, path: &str) -> bool {
        self.lookup(path).is_some()
    }
}

impl Default for MemFileSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ── Vfs dispatcher ──────────────────────────────────────────────

struct MountEntry {
    mount_point: String,
    fs: Box<dyn FileSystem>,
}

pub struct Vfs {
    mounts: Vec<MountEntry>,
    wd: String,
}

impl Vfs {
    /// Resolve a path to its mount index and remaining path without borrowing
    /// the filesystem.  The caller can then use `fs_at()` or `fs_ptr_at()` to
    /// access the filesystem without holding the VFS lock.
    pub fn find_fs_for_path(&self, path: &str) -> Option<(usize, String)> {
        let absolute = self.resolve_path(path);
        let index = self.find_fs_index_for_absolute_path(&absolute)?;
        let remaining = relative_to_mount(&absolute, &self.mounts[index].mount_point)?.to_string();
        Some((index, remaining))
    }

    /// Return a raw pointer to the filesystem at the given mount index.
    /// The caller must ensure no aliasing violations — this exists solely for
    /// callers that need to drop the VFS lock before issuing I/O.
    pub fn fs_ptr_at(&mut self, index: usize) -> *mut Box<dyn FileSystem> {
        &mut self.mounts[index].fs
    }

    pub fn new(root_fs: Box<dyn FileSystem>) -> Self {
        let mounts = alloc::vec![MountEntry {
            mount_point: String::from("/"),
            fs: root_fs,
        }];
        Self {
            mounts,
            wd: String::from("/"),
        }
    }

    pub fn working_directory(&self) -> &str {
        &self.wd
    }

    pub fn change_directory(&mut self, path: &str) -> Result<(), FsError> {
        let resolved = self.resolve_path(path);
        let (fs, remaining) = self.find_fs(&resolved).ok_or(FsError::FileNotFound)?;
        let _entries = fs.readdir(&remaining)?;
        self.wd = resolved;
        Ok(())
    }

    pub fn mount(&mut self, mount_point: &str, fs: Box<dyn FileSystem>) -> Result<(), FsError> {
        let mp = normalize_path(mount_point);
        if mp != "/" {
            let (target_fs, remaining) = self.find_fs(&mp).ok_or(FsError::FileNotFound)?;
            target_fs.readdir(&remaining).map_err(|e| {
                if e == FsError::NotADirectory {
                    FsError::NotADirectory
                } else {
                    FsError::FileNotFound
                }
            })?;
        }
        if let Some(entry) = self.mounts.iter_mut().find(|m| m.mount_point == mp) {
            entry.fs = fs;
        } else {
            self.mounts.push(MountEntry {
                mount_point: mp,
                fs,
            });
        }
        Ok(())
    }

    pub fn unmount(&mut self, mount_point: &str) -> Result<bool, FsError> {
        let mp = normalize_path(mount_point);
        if mp == "/" {
            return Err(FsError::InvalidInput);
        }
        let len_before = self.mounts.len();
        self.mounts.retain(|m| m.mount_point != mp);
        Ok(self.mounts.len() < len_before)
    }

    pub fn find_fs_index(&self, path: &str) -> Option<usize> {
        let absolute_path = self.resolve_path(path);
        self.find_fs_index_for_absolute_path(&absolute_path)
    }

    /// Return the index of a filesystem mounted exactly at `mount_point`.
    pub fn mounted_fs_index(&self, mount_point: &str) -> Option<usize> {
        let mount_point = normalize_path(mount_point);
        self.mounts
            .iter()
            .position(|entry| entry.mount_point == mount_point)
    }

    pub fn resolve_path(&self, path: &str) -> String {
        if path.starts_with('/') {
            normalize_path(path)
        } else if path.is_empty() {
            self.wd.clone()
        } else {
            let mut base = if self.wd.ends_with('/') {
                self.wd.clone()
            } else {
                alloc::format!("{}/", self.wd)
            };
            base.push_str(path);
            normalize_path(&base)
        }
    }

    pub fn find_fs(&mut self, absolute_path: &str) -> Option<(&mut Box<dyn FileSystem>, String)> {
        let path = if absolute_path.starts_with('/') {
            absolute_path
        } else {
            return None;
        };
        let index = self.find_fs_index_for_absolute_path(path)?;
        let remaining = relative_to_mount(path, &self.mounts[index].mount_point)?.to_string();
        Some((&mut self.mounts.get_mut(index)?.fs, remaining))
    }

    fn find_fs_index_for_absolute_path(&self, path: &str) -> Option<usize> {
        if !path.starts_with('/') {
            return None;
        }
        self.mounts
            .iter()
            .enumerate()
            .filter(|(_, entry)| relative_to_mount(path, &entry.mount_point).is_some())
            .max_by_key(|(_, entry)| entry.mount_point.len())
            .map(|(index, _)| index)
    }

    fn resolve_and_find(&mut self, path: &str) -> Option<(&mut Box<dyn FileSystem>, String)> {
        self.find_fs(&self.resolve_path(path))
    }

    fn with_fs<T>(
        &mut self,
        path: &str,
        f: impl FnOnce(&mut Box<dyn FileSystem>, &str) -> Option<T>,
    ) -> Option<T> {
        let (fs, p) = self.resolve_and_find(path)?;
        f(fs, &p)
    }

    fn with_fs_result<T>(
        &mut self,
        path: &str,
        f: impl FnOnce(&mut Box<dyn FileSystem>, &str) -> Result<T, FsError>,
    ) -> Result<T, FsError> {
        let (fs, p) = self.resolve_and_find(path).ok_or(FsError::FileNotFound)?;
        f(fs, &p)
    }

    pub fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor> {
        self.with_fs(path, |fs, p| fs.open(p, flags))
    }

    pub fn read_at(&mut self, mount_idx: usize, fd: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        self.mounts
            .get_mut(mount_idx)
            .ok_or(FsError::InvalidFileDescriptor)?
            .fs
            .read(fd, buf)
    }

    pub fn write_at(&mut self, mount_idx: usize, fd: u32, data: &[u8]) -> Result<usize, FsError> {
        self.mounts
            .get_mut(mount_idx)
            .ok_or(FsError::InvalidFileDescriptor)?
            .fs
            .write(fd, data)
    }

    pub fn close_at(&mut self, mount_idx: usize, fd: u32) -> Result<(), FsError> {
        self.mounts
            .get_mut(mount_idx)
            .ok_or(FsError::InvalidFileDescriptor)?
            .fs
            .close(fd)
    }

    pub fn seek_at(&mut self, mount_idx: usize, fd: u32, pos: u64) -> Result<(), FsError> {
        self.mounts
            .get_mut(mount_idx)
            .ok_or(FsError::InvalidFileDescriptor)?
            .fs
            .seek(fd, pos)
    }

    pub fn position_at(&mut self, mount_idx: usize, fd: u32) -> Result<u64, FsError> {
        self.mounts
            .get_mut(mount_idx)
            .ok_or(FsError::InvalidFileDescriptor)?
            .fs
            .position(fd)
    }

    pub fn size_at(&mut self, mount_idx: usize, fd: u32) -> Result<u64, FsError> {
        self.mounts
            .get_mut(mount_idx)
            .ok_or(FsError::InvalidFileDescriptor)?
            .fs
            .size(fd)
    }

    /// Open a file directly on the VFS and expose it as a Genome stream.
    pub fn open_reader<'a>(&'a mut self, path: &str) -> Result<VfsFile<'a>, FsError> {
        let mount_index = self.find_fs_index(path).ok_or(FsError::FileNotFound)?;
        let descriptor = self.open(path, 0).ok_or(FsError::FileNotFound)?;
        Ok(VfsFile {
            vfs: self,
            mount_index,
            fd: descriptor.fd,
            position: descriptor.offset,
        })
    }

    pub fn create(&mut self, path: &str) -> Option<u64> {
        self.with_fs(path, |fs, p| fs.create(p, InodeType::File))
    }

    pub fn mkdir(&mut self, path: &str) -> Result<(), FsError> {
        self.with_fs_result(path, |fs, p| fs.mkdir(p))
    }

    pub fn unlink(&mut self, path: &str) -> Result<(), FsError> {
        self.with_fs_result(path, |fs, p| fs.unlink(p))
    }

    pub fn readdir(&mut self, path: &str) -> Result<Vec<VNode>, FsError> {
        self.with_fs_result(path, |fs, p| fs.readdir(p))
    }

    pub fn exists(&mut self, path: &str) -> bool {
        self.resolve_and_find(path)
            .is_some_and(|(fs, p)| fs.exists(&p))
    }
}

/// A scoped reader for a file opened through [`Vfs`].
pub struct VfsFile<'a> {
    vfs: &'a mut Vfs,
    mount_index: usize,
    fd: u32,
    position: u64,
}

impl Read for VfsFile<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError> {
        let read = self.vfs.read_at(self.mount_index, self.fd, buf)?;
        self.position = self
            .position
            .checked_add(read as u64)
            .ok_or(FsError::InvalidSeek)?;
        Ok(read)
    }
}

impl Seek for VfsFile<'_> {
    fn seek(&mut self, position: SeekFrom) -> Result<u64, FsError> {
        let absolute = match position {
            SeekFrom::Start(offset) => offset,
            SeekFrom::Current(offset) => self
                .position
                .checked_add_signed(offset)
                .ok_or(FsError::InvalidSeek)?,
            SeekFrom::End(offset) => self
                .len()?
                .checked_add_signed(offset)
                .ok_or(FsError::InvalidSeek)?,
        };
        self.vfs.seek_at(self.mount_index, self.fd, absolute)?;
        self.position = absolute;
        Ok(absolute)
    }
}

impl FileReader for VfsFile<'_> {
    fn len(&mut self) -> Result<u64, FsError> {
        self.vfs.size_at(self.mount_index, self.fd)
    }
}

impl Drop for VfsFile<'_> {
    fn drop(&mut self) {
        let _ = self.vfs.close_at(self.mount_index, self.fd);
    }
}

// ── Path utilities ──────────────────────────────────────────────

/// Return the path as seen from a mount point.
///
/// Mount names must end at a component boundary, so `/mnt2` is not routed to
/// a filesystem mounted at `/mnt`.
pub fn relative_to_mount<'a>(path: &'a str, mount_point: &str) -> Option<&'a str> {
    if !path.starts_with('/') {
        return None;
    }
    if mount_point == "/" {
        return path.strip_prefix('/');
    }
    if path == mount_point {
        return Some("");
    }
    path.strip_prefix(mount_point)?.strip_prefix('/')
}

fn normalize_path(path: &str) -> String {
    let mut components: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            _ => components.push(comp),
        }
    }
    if components.is_empty() {
        return String::from("/");
    }
    let mut result = String::from("/");
    for (i, comp) in components.iter().enumerate() {
        result.push_str(comp);
        if i < components.len() - 1 {
            result.push('/');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memfs_round_trips_file_contents() {
        let mut fs = MemFileSystem::new();
        let ino = fs.create("/hello.txt", InodeType::File).unwrap();
        let descriptor = fs.open("/hello.txt", 0).unwrap();

        assert_eq!(descriptor.ino, ino);
        assert_eq!(fs.write(descriptor.fd, b"fullerene"), Ok(9));
        assert_eq!(fs.seek(descriptor.fd, 0), Ok(()));

        let mut data = [0; 9];
        assert_eq!(fs.read(descriptor.fd, &mut data), Ok(9));
        assert_eq!(&data, b"fullerene");
    }

    #[test]
    fn memfs_declares_writable_large_file_capabilities() {
        assert_eq!(
            MemFileSystem::new().capabilities(),
            FileSystemCapabilities::new(false, true, true, true, true)
        );
    }

    #[test]
    fn oversized_u64_offset_is_rejected_before_usize_conversion() {
        let mut fs = MemFileSystem::new();
        fs.create("/large", InodeType::File).unwrap();
        let descriptor = fs.open("/large", 0).unwrap();
        fs.seek(descriptor.fd, u64::MAX).unwrap();
        assert_eq!(fs.write(descriptor.fd, &[1]), Err(FsError::InvalidInput));
    }

    #[test]
    fn memfs_rejects_reading_a_directory_as_a_file() {
        let mut fs = MemFileSystem::new();
        fs.mkdir("/documents").unwrap();
        let descriptor = fs.open("/documents", 0).unwrap();
        let mut data = [0; 1];

        assert_eq!(
            fs.read(descriptor.fd, &mut data),
            Err(FsError::IsADirectory)
        );
    }

    #[test]
    fn mount_requires_an_existing_directory() {
        let mut root = MemFileSystem::new();
        root.create("/file", InodeType::File).unwrap();
        let mut vfs = Vfs::new(Box::new(root));

        assert_eq!(
            vfs.mount("/missing", Box::new(MemFileSystem::new())),
            Err(FsError::FileNotFound)
        );
        assert_eq!(
            vfs.mount("/file", Box::new(MemFileSystem::new())),
            Err(FsError::NotADirectory)
        );
    }

    #[test]
    fn mount_routing_respects_component_boundaries() {
        let mut root = MemFileSystem::new();
        root.mkdir("/mnt").unwrap();
        root.mkdir("/mnt2").unwrap();
        let mut mounted = MemFileSystem::new();
        mounted.create("/inside", InodeType::File).unwrap();
        let mut vfs = Vfs::new(Box::new(root));
        vfs.mount("/mnt", Box::new(mounted)).unwrap();

        let mounted_index = vfs.find_fs_index("/mnt/inside").unwrap();
        let root_index = vfs.find_fs_index("/mnt2").unwrap();
        assert_ne!(mounted_index, root_index);

        let (mounted_fs, relative_path) = vfs.find_fs("/mnt/inside").unwrap();
        assert_eq!(relative_path, "inside");
        assert!(mounted_fs.exists(&relative_path));
    }

    #[test]
    fn mount_routing_prefers_the_most_specific_mount() {
        let mut root = MemFileSystem::new();
        root.mkdir("/mnt").unwrap();
        let mut first_mount = MemFileSystem::new();
        first_mount.mkdir("/nested").unwrap();
        let mut vfs = Vfs::new(Box::new(root));
        vfs.mount("/mnt", Box::new(first_mount)).unwrap();
        let first_mount_index = vfs.mounted_fs_index("/mnt").unwrap();

        let mut nested_mount = MemFileSystem::new();
        nested_mount
            .create("/nested-file", InodeType::File)
            .unwrap();
        vfs.mount("/mnt/nested", Box::new(nested_mount)).unwrap();

        assert_eq!(vfs.mounted_fs_index("/mnt"), Some(first_mount_index));
        assert_ne!(
            vfs.find_fs_index("/mnt/nested/nested-file"),
            Some(first_mount_index)
        );
        let (fs, relative_path) = vfs.find_fs("/mnt/nested/nested-file").unwrap();
        assert_eq!(relative_path, "nested-file");
        assert!(fs.exists(&relative_path));
    }

    #[test]
    fn path_normalization_stays_within_the_root() {
        assert_eq!(normalize_path("/a/./b/../c"), "/a/c");
        assert_eq!(normalize_path("../../../"), "/");
    }

    #[test]
    fn vfs_reader_supports_bounded_stream_operations() {
        let mut vfs = Vfs::new(Box::new(MemFileSystem::new()));
        vfs.create("/stream.bin").unwrap();
        let fd = vfs.open("/stream.bin", 0).unwrap().fd;
        let mount = vfs.find_fs_index("/stream.bin").unwrap();
        assert_eq!(vfs.write_at(mount, fd, b"fullerene"), Ok(9));
        vfs.close_at(mount, fd).unwrap();

        let mut reader = vfs.open_reader("/stream.bin").unwrap();
        assert_eq!(reader.len(), Ok(9));
        let mut prefix = [0u8; 4];
        reader.read_exact(&mut prefix).unwrap();
        assert_eq!(&prefix, b"full");
        assert_eq!(reader.seek(SeekFrom::End(-5)), Ok(4));
        let mut suffix = [0u8; 5];
        reader.read_exact(&mut suffix).unwrap();
        assert_eq!(&suffix, b"erene");
        drop(reader);

        let reopened = vfs.open("/stream.bin", 0).unwrap();
        assert_eq!(reopened.offset, 0);
    }
}
