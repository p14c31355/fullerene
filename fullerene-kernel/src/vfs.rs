//! VFS (Virtual File System) with tmpfs backend.
//!
//! Provides a unified filesystem interface backed by an in‑memory tmpfs
//! for the kernel shell (ls, cat, write).  Mount-point routing delegates
//! to concrete filesystem drivers (tmpfs, FAT32).
//!
//! # Architecture
//!
//! ```
//! syscall (open/read/write/close) → VFS dispatch → tmpfs
//! ```
//!
//! The `tmpfs` is a B‑tree of in‑memory inodes and data blocks stored in
//! the kernel heap.  It supports directories, regular files, and symlinks.

use alloc::collections::BTreeMap;
use alloc::string::String;
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

// ── Inode ───────────────────────────────────────────────────────

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

// ── tmpfs ───────────────────────────────────────────────────────

struct Tmpfs {
    /// Inode table: inode number → Inode.
    inodes: BTreeMap<u64, Inode>,
    next_ino: u64,
    /// Open file descriptors: fd → FileDescriptor.
    fds: BTreeMap<u32, FileDescriptor>,
    next_fd: u32,
}

impl Tmpfs {
    fn new() -> Self {
        let root = Inode::new(1, "", InodeType::Directory, 0);
        let mut inodes = BTreeMap::new();
        inodes.insert(1, root);
        Self { inodes, next_ino: 2, fds: BTreeMap::new(), next_fd: 0 }
    }

    /// Resolve a path into an inode number, traversing directories.
    fn lookup(&self, path: &str) -> Option<u64> {
        self.lookup_impl(path, 0)
    }

    /// Internal lookup with recursion depth guard for symlink loops.
    /// `depth` must be ≤ [`MAX_SYMLINK_DEPTH`].
    fn lookup_impl(&self, path: &str, depth: u32) -> Option<u64> {
        if depth > MAX_SYMLINK_DEPTH {
            return None; // symlink loop detected
        }
        if path.is_empty() || path == "/" {
            return Some(1);
        }
        let components: Vec<&str> = path.trim_start_matches('/').split('/')
            .filter(|c| !c.is_empty()).collect();
        let mut current = 1u64; // root
        for comp in components {
            let ino = self.inodes.get(&current)?;
            let child = ino.children.iter()
                .find(|&&c| self.inodes.get(&c).map_or(false, |i| i.name.as_str() == comp))?;
            current = *child;
            // Follow symlinks with depth guard
            if let Some(ref target) = self.inodes.get(&current)?.target {
                return self.lookup_impl(target, depth + 1);
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
        let last_slash = path.rfind('/')?;
        let parent_path = if last_slash == 0 { "/" } else { &path[..last_slash] };
        let name = String::from(&path[last_slash + 1..]);
        let parent_ino = self.lookup(parent_path)?;
        Some((parent_ino, name))
    }

    fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor> {
        let ino = self.lookup(path)?;
        let fd = self.next_fd;
        self.next_fd += 1;
        let desc = FileDescriptor { fd, ino, offset: 0, flags };
        self.fds.insert(fd, desc.clone());
        Some(desc)
    }

    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, &'static str> {
        let desc = self.fds.get_mut(&fd).ok_or("bad fd")?;
        let ino = self.inodes.get(&desc.ino).ok_or("inode not found")?;
        // Guard against offset beyond EOF (e.g. after truncation).
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
        let off = desc.offset;
        if off + data.len() > ino.data.len() {
            ino.data.resize(off + data.len(), 0);
        }
        ino.data[off..off + data.len()].copy_from_slice(data);
        ino.size = ino.data.len();
        desc.offset += data.len();
        Ok(data.len())
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
        let ino = self.next_ino;
        self.next_ino = ino + 1;
        let inode = Inode::new(ino, &name, kind, parent_ino);
        self.inodes.insert(ino, inode);
        if let Some(parent) = self.inodes.get_mut(&parent_ino) {
            parent.children.push(ino);
        }
        Some(ino)
    }

    fn lookup_child(&self, parent_ino: u64, name: &str) -> Option<u64> {
        let parent = self.inodes.get(&parent_ino)?;
        parent.children.iter()
            .find(|&&c| self.inodes.get(&c).map_or(false, |i| i.name.as_str() == name))
            .copied()
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

    fn unlink(&mut self, path: &str) -> Result<(), &'static str> {
        let (parent_ino, name) = self.lookup_parent(path).ok_or("not found")?;
        let child_ino = self.lookup_child(parent_ino, &name).ok_or("not found")?;
        if let Some(parent) = self.inodes.get_mut(&parent_ino) {
            parent.children.retain(|&c| c != child_ino);
        }
        self.inodes.remove(&child_ino);
        Ok(())
    }
}

// ── Global VFS state ────────────────────────────────────────────

static VFS: Mutex<Option<Tmpfs>> = Mutex::new(None);

fn vfs() -> &'static Mutex<Option<Tmpfs>> {
    &VFS
}

// ── Public API ──────────────────────────────────────────────────

pub fn init() {
    let mut guard = VFS.lock();
    *guard = Some(Tmpfs::new());
    log::info!("VFS: tmpfs mounted at /");
}

pub fn mount(_device: &str, _mount_point: &str, _fs_type: &str) -> Result<(), &'static str> {
    // Only tmpfs is supported currently
    if _fs_type == "tmpfs" {
        log::info!("VFS: mount tmpfs at {} — already mounted", _mount_point);
        return Ok(());
    }
    Err("only tmpfs is supported")
}

pub fn open(path: &str, flags: u32) -> Result<FileDescriptor, &'static str> {
    vfs().lock().as_mut().ok_or("vfs not init")?
        .open(path, flags).ok_or("not found")
}

pub fn read(fd: u32, buf: &mut [u8]) -> Result<usize, &'static str> {
    vfs().lock().as_mut().ok_or("vfs not init")?.read(fd, buf)
}

pub fn write(fd: u32, data: &[u8]) -> Result<usize, &'static str> {
    vfs().lock().as_mut().ok_or("vfs not init")?.write(fd, data)
}

pub fn close(fd: u32) -> Result<(), &'static str> {
    let mut guard = vfs().lock();
    let fs = guard.as_mut().ok_or("vfs not init")?;
    fs.fds.remove(&fd).ok_or("bad fd")?;
    Ok(())
}

pub fn readdir(path: &str) -> Result<Vec<VNode>, &'static str> {
    vfs().lock().as_mut().ok_or("vfs not init")?.readdir(path)
}

pub fn create(path: &str) -> Result<FileDescriptor, &'static str> {
    let fs = vfs().lock();
    let fs = fs.as_ref().ok_or("vfs not init")?;
    // Create a regular file if none exists, then open it
    let ino = fs.lookup(path);
    if ino.is_none() {
        drop(fs);
        vfs().lock().as_mut().unwrap().create(path, InodeType::File).ok_or("create failed")?;
    }
    vfs().lock().as_mut().ok_or("vfs not init")?
        .open(path, 0).ok_or("open failed after create")
}

pub fn mkdir(path: &str) -> Result<(), &'static str> {
    let mut guard = vfs().lock();
    let fs = guard.as_mut().ok_or("vfs not init")?;
    // Check if path is root
    if path == "/" { return Ok(()); }
    let (_, _) = fs.lookup_parent(path).ok_or("invalid path")?;
    fs.create(path, InodeType::Directory).ok_or("mkdir failed")?;
    Ok(())
}

pub fn unlink(path: &str) -> Result<(), &'static str> {
    vfs().lock().as_mut().ok_or("vfs not init")?.unlink(path)
}
