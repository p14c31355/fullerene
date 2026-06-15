//! VFS (Virtual File System) with mount-point routing.
//!
//! Provides a unified filesystem interface through the [`FileSystem`] trait.
//! The [`Vfs`] dispatcher routes path-based operations to the correct
//! filesystem driver based on the mount table.
//!
//! # Architecture
//!
//! ```text
//! syscall (open/read/write/close)
//!   → VFS dispatch (mount-table routing)
//!     → MemFileSystem  (in-memory tmpfs, default root)
//!     → FatFileSystem  (FAT32, future)
//! ```
//!
//! # Mount table
//!
//! Every filesystem is mounted at a specific path prefix.  The dispatcher
//! selects the filesystem whose mount point is the longest prefix of the
//! requested path.  The root filesystem is always mounted at `"/"`.
//!
//! # Working directory
//!
//! The VFS maintains a current working directory (`wd`).  Relative paths
//! are resolved against `wd` before being dispatched.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

/// Maximum depth for symbolic link resolution to prevent infinite loops.
const MAX_SYMLINK_DEPTH: u32 = 8;

// ── Inode types ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InodeType {
    File,
    Directory,
    Symlink,
}

// ── Inode (internal to MemFileSystem) ───────────────────────────

#[derive(Debug, Clone)]
struct Inode {
    ino: u64,
    name: String,
    kind: InodeType,
    /// File data (empty for directories).
    data: Vec<u8>,
    /// Children inode numbers (directories only).
    children: Vec<u64>,
    /// Parent inode number (0 = root).
    parent: u64,
    /// Symlink target path.
    target: Option<String>,
    /// File size (cached from data.len()).
    size: usize,
}

impl Inode {
    fn new(ino: u64, name: &str, kind: InodeType, parent: u64) -> Self {
        Self {
            ino,
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

// ── File descriptor ─────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FileDescriptor {
    pub fd: u32,
    pub ino: u64,
    pub offset: usize,
    pub flags: u32,
}

// ── Public-facing VNode ─────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct VNode {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

// ── FileSystem trait ────────────────────────────────────────────

/// Abstract filesystem interface.
///
/// Every concrete filesystem (tmpfs, FAT32, initrd, …) implements this
/// trait.  The [`Vfs`] dispatcher routes operations to the correct
/// implementation based on the mount table.
///
/// All methods receive `&mut self` — the caller (typically [`Vfs`])
/// holds the global lock, so no additional synchronisation is required.
pub trait FileSystem: Send {
    /// Open a file at `path` and return a file descriptor.
    fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor>;

    /// Read from an open file descriptor into `buf`.
    /// Returns the number of bytes read (`0` = EOF).
    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, &'static str>;

    /// Write `data` to an open file descriptor.
    /// Returns the number of bytes written.
    fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, &'static str>;

    /// Close a file descriptor.
    fn close(&mut self, fd: u32) -> Result<(), &'static str>;

    /// Seek to `pos` in an open file descriptor.
    fn seek(&mut self, fd: u32, pos: usize) -> Result<(), &'static str>;

    /// Create a new file or directory at `path`.
    /// Returns the new inode number.
    fn create(&mut self, path: &str, kind: InodeType) -> Option<u64>;

    /// Create a directory at `path`.
    fn mkdir(&mut self, path: &str) -> Result<(), &'static str>;

    /// Remove a file or empty directory at `path`.
    fn unlink(&mut self, path: &str) -> Result<(), &'static str>;

    /// List directory contents.
    fn readdir(&self, path: &str) -> Result<Vec<VNode>, &'static str>;

    /// Check whether `path` exists.
    fn exists(&self, path: &str) -> bool;
}

// ── MemFileSystem (in-memory tmpfs) ─────────────────────────────

/// An in-memory filesystem backed by a B‑tree of inodes.
///
/// Supports directories, regular files, and symlinks.  All data is
/// stored in the kernel heap.
pub struct MemFileSystem {
    /// Inode table: inode number → Inode.
    inodes: BTreeMap<u64, Inode>,
    next_ino: u64,
    /// Open file descriptors: fd → FileDescriptor.
    fds: BTreeMap<u32, FileDescriptor>,
    next_fd: u32,
}

impl MemFileSystem {
    /// Create a new empty memory filesystem with a root directory.
    pub fn new() -> Self {
        let root = Inode::new(1, "", InodeType::Directory, 0);
        let mut inodes = BTreeMap::new();
        inodes.insert(1, root);
        Self {
            inodes,
            next_ino: 2,
            fds: BTreeMap::new(),
            next_fd: 0,
        }
    }

    // ── Internal helpers ────────────────────────────────────────

    /// Resolve a path into an inode number, traversing directories.
    fn lookup(&self, path: &str) -> Option<u64> {
        self.lookup_from(path, 1, 0)
    }

    /// Internal lookup with configurable starting inode and recursion
    /// depth guard for symlink loops.
    fn lookup_from(&self, path: &str, start_ino: u64, depth: u32) -> Option<u64> {
        if depth > MAX_SYMLINK_DEPTH {
            return None;
        }
        if path.is_empty() {
            return Some(start_ino);
        }
        let (effective_start, trimmed) = if path.starts_with('/') {
            (1u64, path.trim_start_matches('/'))
        } else {
            (start_ino, path)
        };
        if trimmed.is_empty() {
            return Some(effective_start);
        }
        let components: Vec<&str> = trimmed.split('/').filter(|c| !c.is_empty()).collect();
        if components.is_empty() {
            return Some(effective_start);
        }
        let mut current = effective_start;
        for (idx, comp) in components.iter().enumerate() {
            let parent_ino = current;
            let ino = self.inodes.get(&current)?;
            let child = ino.children.iter().find(|&&c| {
                self.inodes
                    .get(&c)
                    .map_or(false, |i| i.name.as_str() == *comp)
            })?;
            current = *child;
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

    /// Resolve a parent directory and the final component name.
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
            .find(|&&c| {
                self.inodes
                    .get(&c)
                    .map_or(false, |i| i.name.as_str() == name)
            })
            .copied()
    }

    /// Recursively collect all descendant inode numbers of a directory.
    fn collect_descendants(&self, dir_ino: u64) -> Vec<u64> {
        let mut result = Vec::new();
        let Some(inode) = self.inodes.get(&dir_ino) else {
            return result;
        };
        for &c in &inode.children {
            result.push(c);
            result.extend(self.collect_descendants(c));
        }
        result
    }
}

impl FileSystem for MemFileSystem {
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

    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, &'static str> {
        let desc = self.fds.get_mut(&fd).ok_or("bad fd")?;
        let ino = self.inodes.get(&desc.ino).ok_or("inode not found")?;
        if desc.offset >= ino.data.len() {
            return Ok(0);
        }
        let data = &ino.data[desc.offset..];
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        desc.offset += n;
        Ok(n)
    }

    fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, &'static str> {
        let desc = self.fds.get_mut(&fd).ok_or("bad fd")?;
        let ino = self.inodes.get_mut(&desc.ino).ok_or("inode not found")?;
        if ino.kind != InodeType::File {
            return Err("not a file");
        }
        let off = desc.offset;
        if off + data.len() > ino.data.len() {
            ino.data.resize(off + data.len(), 0);
        }
        ino.data[off..off + data.len()].copy_from_slice(data);
        ino.size = ino.data.len();
        desc.offset += data.len();
        Ok(data.len())
    }

    fn close(&mut self, fd: u32) -> Result<(), &'static str> {
        self.fds.remove(&fd).ok_or("bad fd")?;
        Ok(())
    }

    fn seek(&mut self, fd: u32, pos: usize) -> Result<(), &'static str> {
        let desc = self.fds.get_mut(&fd).ok_or("bad fd")?;
        desc.offset = pos;
        Ok(())
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
        let inode = Inode::new(ino, &name, kind, parent_ino);
        self.inodes.insert(ino, inode);
        if let Some(parent) = self.inodes.get_mut(&parent_ino) {
            parent.children.push(ino);
        }
        Some(ino)
    }

    fn mkdir(&mut self, path: &str) -> Result<(), &'static str> {
        if path == "/" {
            return Ok(());
        }
        let (_, _) = self.lookup_parent(path).ok_or("invalid path")?;
        self.create(path, InodeType::Directory)
            .ok_or("mkdir failed")?;
        Ok(())
    }

    fn unlink(&mut self, path: &str) -> Result<(), &'static str> {
        let (parent_ino, name) = self.lookup_parent(path).ok_or("not found")?;
        let child_ino = self.lookup_child(parent_ino, &name).ok_or("not found")?;
        let child = self.inodes.get(&child_ino).ok_or("not found")?;
        if child.kind == InodeType::Directory && !child.children.is_empty() {
            return Err("directory not empty");
        }
        if let Some(parent) = self.inodes.get_mut(&parent_ino) {
            parent.children.retain(|&c| c != child_ino);
        }
        self.inodes.remove(&child_ino);
        Ok(())
    }

    fn readdir(&self, path: &str) -> Result<Vec<VNode>, &'static str> {
        let ino = self.lookup(path).ok_or("not found")?;
        let inode = self.inodes.get(&ino).ok_or("not found")?;
        let mut entries = Vec::new();
        for &c in &inode.children {
            if let Some(child) = self.inodes.get(&c) {
                entries.push(VNode {
                    name: child.name.clone(),
                    size: child.size as u64,
                    is_dir: child.kind == InodeType::Directory,
                });
            }
        }
        Ok(entries)
    }

    fn exists(&self, path: &str) -> bool {
        self.lookup(path).is_some()
    }
}

// ── Vfs dispatcher ──────────────────────────────────────────────

/// Mount-table entry: maps a mount-point path to a filesystem.
struct MountEntry {
    /// Mount-point path (e.g. `"/"`, `"/mnt/fat"`).
    mount_point: String,
    /// The mounted filesystem.
    fs: Box<dyn FileSystem>,
}

/// Virtual File System dispatcher.
///
/// Holds the mount table and routes path-based operations to the
/// correct [`FileSystem`] implementation.
pub struct Vfs {
    /// Mount table sorted by mount-point length (longest first for
    /// correct prefix matching).
    mounts: Vec<MountEntry>,
    /// Current working directory.
    wd: String,
}

impl Vfs {
    /// Create a new VFS with the given root filesystem.
    pub fn new(root_fs: Box<dyn FileSystem>) -> Self {
        let mut mounts = Vec::new();
        mounts.push(MountEntry {
            mount_point: String::from("/"),
            fs: root_fs,
        });
        Self {
            mounts,
            wd: String::from("/"),
        }
    }

    // ── Working directory ───────────────────────────────────────

    /// Get the current working directory.
    pub fn working_directory(&self) -> &str {
        &self.wd
    }

    /// Set the current working directory.
    ///
    /// Returns an error if `path` does not exist or is not a directory.
    pub fn change_directory(&mut self, path: &str) -> Result<(), &'static str> {
        let resolved = self.resolve_path(path);
        // Verify the path exists and is a directory.
        let (fs, remaining) = self.find_fs(&resolved).ok_or("not found")?;
        let _entries = fs.readdir(&remaining).map_err(|_| "not a directory")?;
        self.wd = resolved;
        Ok(())
    }

    // ── Mount management ────────────────────────────────────────

    /// Mount a filesystem at `mount_point`.
    ///
    /// The mount point must be an existing directory.  The filesystem
    /// will receive paths relative to the mount point.
    pub fn mount(
        &mut self,
        mount_point: &str,
        fs: Box<dyn FileSystem>,
    ) -> Result<(), &'static str> {
        let mp = normalize_path(mount_point);
        // Ensure the mount point exists
        if mp != "/" {
            // Check existence of the mount point directory.
            let (target_fs, remaining) = self.find_fs(&mp).ok_or("mount point not found")?;
            if !target_fs.exists(&remaining) {
                return Err("mount point not found");
            }
        }
        // Remove any existing mount at the same point.
        self.mounts.retain(|m| m.mount_point != mp);
        self.mounts.push(MountEntry {
            mount_point: mp,
            fs,
        });
        // Sort by mount-point length descending so longest prefix
        // matches first.
        self.mounts
            .sort_by(|a, b| b.mount_point.len().cmp(&a.mount_point.len()));
        Ok(())
    }

    // ── Path resolution ─────────────────────────────────────────

    /// Resolve a potentially relative path to an absolute one.
    fn resolve_path(&self, path: &str) -> String {
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

    /// Find the filesystem responsible for `absolute_path`.
    ///
    /// Returns `(filesystem, remaining_path_relative_to_mount_point)`.
    fn find_fs(&mut self, absolute_path: &str) -> Option<(&mut Box<dyn FileSystem>, String)> {
        // Ensure path starts with /
        let path = if absolute_path.starts_with('/') {
            absolute_path
        } else {
            return None;
        };

        for entry in &mut self.mounts {
            let mp = &entry.mount_point;
            if mp == "/" {
                // Root matches everything.
                let remaining = path[1..].to_string();
                // Use unsafe pointer cast to split the borrow.  The
                // mount entries are distinct so this is safe.
                let fs: *mut Box<dyn FileSystem> = &mut entry.fs;
                return Some((unsafe { &mut *fs }, remaining));
            }
            let mp_prefix = mp.as_str();
            let mp_with_slash = alloc::format!("{}/", mp_prefix);
            if path == mp_prefix || path.starts_with(&mp_with_slash) {
                let remaining = if path == mp_prefix {
                    String::new()
                } else {
                    path[mp_with_slash.len()..].to_string()
                };
                let fs: *mut Box<dyn FileSystem> = &mut entry.fs;
                return Some((unsafe { &mut *fs }, remaining));
            }
        }
        None
    }

    // ── Operations ──────────────────────────────────────────────

    pub fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor> {
        let resolved = self.resolve_path(path);
        let (fs, remaining) = self.find_fs(&resolved)?;
        fs.open(&remaining, flags)
    }

    pub fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, &'static str> {
        // Search all filesystems for the fd.
        for entry in &mut self.mounts {
            if let Ok(n) = entry.fs.read(fd, buf) {
                return Ok(n);
            }
        }
        Err("bad fd")
    }

    pub fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, &'static str> {
        for entry in &mut self.mounts {
            if let Ok(n) = entry.fs.write(fd, data) {
                return Ok(n);
            }
        }
        Err("bad fd")
    }

    pub fn close(&mut self, fd: u32) -> Result<(), &'static str> {
        for entry in &mut self.mounts {
            if entry.fs.close(fd).is_ok() {
                return Ok(());
            }
        }
        Err("bad fd")
    }

    pub fn seek(&mut self, fd: u32, pos: usize) -> Result<(), &'static str> {
        for entry in &mut self.mounts {
            if entry.fs.seek(fd, pos).is_ok() {
                return Ok(());
            }
        }
        Err("bad fd")
    }

    pub fn create(&mut self, path: &str) -> Option<u64> {
        let resolved = self.resolve_path(path);
        let (fs, remaining) = self.find_fs(&resolved)?;
        fs.create(&remaining, InodeType::File)
    }

    pub fn mkdir(&mut self, path: &str) -> Result<(), &'static str> {
        let resolved = self.resolve_path(path);
        let (fs, remaining) = self.find_fs(&resolved).ok_or("not found")?;
        fs.mkdir(&remaining)
    }

    pub fn unlink(&mut self, path: &str) -> Result<(), &'static str> {
        let resolved = self.resolve_path(path);
        let (fs, remaining) = self.find_fs(&resolved).ok_or("not found")?;
        fs.unlink(&remaining)
    }

    pub fn readdir(&mut self, path: &str) -> Result<Vec<VNode>, &'static str> {
        let resolved = self.resolve_path(path);
        let (fs, remaining) = self.find_fs(&resolved).ok_or("not found")?;
        fs.readdir(&remaining)
    }

    pub fn exists(&mut self, path: &str) -> bool {
        let resolved = self.resolve_path(path);
        match self.find_fs(&resolved) {
            Some((fs, remaining)) => fs.exists(&remaining),
            None => false,
        }
    }
}

// ── Path utilities ──────────────────────────────────────────────

/// Normalise a path by collapsing `.`, `..`, and double slashes.
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

// ── Global VFS state ────────────────────────────────────────────

static VFS: Mutex<Option<Vfs>> = Mutex::new(None);

pub(crate) fn vfs() -> &'static Mutex<Option<Vfs>> {
    &VFS
}

// ── Public API (backward-compatible with pre-trait code) ─────────

/// Initialise the VFS with a memory-backed root filesystem.
pub fn init() {
    let root_fs = Box::new(MemFileSystem::new());
    let mut guard = VFS.lock();
    *guard = Some(Vfs::new(root_fs));
    log::info!("VFS: mounted MemFileSystem at /");
}

/// Mount a filesystem at `mount_point`.
///
/// Currently only `tmpfs` type is supported (creates a new
/// [`MemFileSystem`]).
pub fn mount(device: &str, mount_point: &str, fs_type: &str) -> Result<(), &'static str> {
    let mut guard = vfs().lock();
    let vfs = guard.as_mut().ok_or("vfs not init")?;
    match fs_type {
        "tmpfs" => {
            let memfs = Box::new(MemFileSystem::new());
            vfs.mount(mount_point, memfs)?;
            log::info!("VFS: mounted tmpfs from {} at {}", device, mount_point);
            Ok(())
        }
        _ => Err("unsupported filesystem type"),
    }
}

/// Open a file at `path` and return a file descriptor.
pub fn open(path: &str, flags: u32) -> Result<FileDescriptor, &'static str> {
    vfs()
        .lock()
        .as_mut()
        .ok_or("vfs not init")?
        .open(path, flags)
        .ok_or("not found")
}

/// Read from an open file descriptor into `buf`.
pub fn read(fd: u32, buf: &mut [u8]) -> Result<usize, &'static str> {
    vfs().lock().as_mut().ok_or("vfs not init")?.read(fd, buf)
}

/// Write `data` to an open file descriptor.
pub fn write(fd: u32, data: &[u8]) -> Result<usize, &'static str> {
    vfs().lock().as_mut().ok_or("vfs not init")?.write(fd, data)
}

/// Close a file descriptor.
pub fn close(fd: u32) -> Result<(), &'static str> {
    vfs().lock().as_mut().ok_or("vfs not init")?.close(fd)
}

/// List directory contents at `path`.
pub fn readdir(path: &str) -> Result<Vec<VNode>, &'static str> {
    vfs().lock().as_mut().ok_or("vfs not init")?.readdir(path)
}

/// Seek to `pos` in an open file descriptor.
pub fn seek(fd: u32, pos: usize) -> Result<(), &'static str> {
    vfs().lock().as_mut().ok_or("vfs not init")?.seek(fd, pos)
}

/// Create a regular file at `path` (or open existing), returning a fd.
pub fn create(path: &str) -> Result<FileDescriptor, &'static str> {
    let mut guard = vfs().lock();
    let vfs = guard.as_mut().ok_or("vfs not init")?;
    let resolved = vfs.resolve_path(path);
    {
        let (fs, remaining) = vfs.find_fs(&resolved).ok_or("not found")?;
        if !fs.exists(&remaining) {
            fs.create(&remaining, InodeType::File)
                .ok_or("create failed")?;
        }
    }
    // Re-resolve to open the created file.
    let (fs, remaining) = vfs.find_fs(&resolved).ok_or("not found")?;
    fs.open(&remaining, 0).ok_or("open failed after create")
}

/// Create a directory at `path`.
pub fn mkdir(path: &str) -> Result<(), &'static str> {
    vfs().lock().as_mut().ok_or("vfs not init")?.mkdir(path)
}

/// Remove a file or empty directory at `path`.
pub fn unlink(path: &str) -> Result<(), &'static str> {
    vfs().lock().as_mut().ok_or("vfs not init")?.unlink(path)
}

/// Check whether `path` exists.
pub fn exists(path: &str) -> bool {
    match vfs().lock().as_mut() {
        Some(vfs) => vfs.exists(path),
        None => false,
    }
}

/// Get the current working directory.
pub fn working_directory() -> Result<String, &'static str> {
    let guard = vfs().lock();
    let vfs = guard.as_ref().ok_or("vfs not init")?;
    Ok(String::from(vfs.working_directory()))
}

/// Change the current working directory.
pub fn change_directory(path: &str) -> Result<(), &'static str> {
    vfs()
        .lock()
        .as_mut()
        .ok_or("vfs not init")?
        .change_directory(path)
}
