//! VfsContext — unified VFS state that replaces the scattered `vfs::open()` /
//! `vfs::read()` / `vfs::cwd` / `vfs::mount` global calls.
//!
//! Holds mount table, working directory, and open-handle routing inside a single
//! context struct so that higher layers access the VFS through
//! `kernel.vfs.open(path)` rather than scattered module-level statics.
//!
//! # Locking
//!
//! `VfsContext` uses internal `spin::Mutex` (upgradable to finer-grained
//! locking later).  It lives inside `KernelContext` which is itself behind a
//! global lock; the inner lock is still required because `VfsContext` may be
//! accessed independently from the kernel-level lock in the future.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use crate::vfs::{FileDescriptor, FileSystem, InodeType, MemFileSystem, VNode, Vfs};

// ── VfsContext ──────────────────────────────────────────────────────

/// Virtual File System context bundling mount table, cwd, and a handle
/// table for open file descriptors.
///
/// ```ignore
/// kernel_context().with_mut(|k| {
///     k.vfs.open("/etc/hostname", 0);
///     k.vfs.readdir("/");
/// });
/// ```
pub struct VfsContext {
    inner: Mutex<Vfs>,

    /// Per-filesystem open-handle tracking so that `read(fd)` /
    /// `close(fd)` routes to the correct filesystem without scanning
    /// every mount every time.  Maps fd → index into `Vfs.mounts`.
    handle_table: Mutex<HandleTable>,
    next_handle_id: Mutex<u32>,
}

/// Tracks which mount owns each open fd.
struct HandleTable {
    entries: Vec<HandleEntry>,
}

struct HandleEntry {
    fd: u32,
    mount_index: usize,
}

impl HandleTable {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn insert(&mut self, fd: u32, mount_index: usize) {
        self.entries.push(HandleEntry { fd, mount_index });
    }

    fn find(&self, fd: u32) -> Option<usize> {
        self.entries
            .iter()
            .find(|e| e.fd == fd)
            .map(|e| e.mount_index)
    }

    fn remove(&mut self, fd: u32) {
        self.entries.retain(|e| e.fd != fd);
    }
}

unsafe impl Send for VfsContext {}
unsafe impl Sync for VfsContext {}

impl VfsContext {
    /// Create a new VFS context with a memory-backed root filesystem.
    pub fn new() -> Self {
        let root_fs = Box::new(MemFileSystem::new());
        Self {
            inner: Mutex::new(Vfs::new(root_fs)),
            handle_table: Mutex::new(HandleTable::new()),
            next_handle_id: Mutex::new(0),
        }
    }

    // ── Working directory ───────────────────────────────────────

    pub fn working_directory(&self) -> String {
        let vfs = self.inner.lock();
        String::from(vfs.working_directory())
    }

    pub fn change_directory(&self, path: &str) -> Result<(), &'static str> {
        self.inner.lock().change_directory(path)
    }

    // ── Mount management ────────────────────────────────────────

    pub fn mount(
        &self,
        mount_point: &str,
        fs: Box<dyn FileSystem>,
    ) -> Result<(), &'static str> {
        self.inner.lock().mount(mount_point, fs)
    }

    // ── File operations ─────────────────────────────────────────

    pub fn open(&self, path: &str, flags: u32) -> Option<FileDescriptor> {
        let mut vfs = self.inner.lock();
        let mount_index = vfs.find_fs_index(path)?;
        let fd = vfs.open(path, flags)?;

        // Track which mount owns this fd for fast read/write/close routing.
        self.handle_table.lock().insert(fd.fd, mount_index);
        Some(fd)
    }

    pub fn read(&self, fd: u32, buf: &mut [u8]) -> Result<usize, &'static str> {
        let mount_idx = self
            .handle_table
            .lock()
            .find(fd)
            .ok_or("bad fd")?;
        self.inner.lock().read_at(mount_idx, fd, buf)
    }

    pub fn write(&self, fd: u32, data: &[u8]) -> Result<usize, &'static str> {
        let mount_idx = self
            .handle_table
            .lock()
            .find(fd)
            .ok_or("bad fd")?;
        self.inner.lock().write_at(mount_idx, fd, data)
    }

    pub fn close(&self, fd: u32) -> Result<(), &'static str> {
        let mount_idx = self
            .handle_table
            .lock()
            .find(fd)
            .ok_or("bad fd")?;
        self.inner.lock().close_at(mount_idx, fd)?;
        self.handle_table.lock().remove(fd);
        Ok(())
    }

    pub fn seek(&self, fd: u32, pos: usize) -> Result<(), &'static str> {
        let mount_idx = self
            .handle_table
            .lock()
            .find(fd)
            .ok_or("bad fd")?;
        self.inner.lock().seek_at(mount_idx, fd, pos)
    }

    pub fn create(&self, path: &str) -> Result<FileDescriptor, &'static str> {
        let mut vfs = self.inner.lock();
        let mount_index = vfs
            .find_fs_index(path)
            .ok_or("not found")?;
        let resolved = vfs.resolve_path(path);
        {
            let (fs, remaining) = vfs
                .find_fs(&resolved)
                .ok_or("not found")?;
            if !fs.exists(&remaining) {
                fs.create(&remaining, InodeType::File)
                    .ok_or("create failed")?;
            }
        }
        let (fs, remaining) = vfs.find_fs(&resolved).ok_or("not found")?;
        let fd = fs.open(&remaining, 0).ok_or("open failed after create")?;
        self.handle_table.lock().insert(fd.fd, mount_index);
        Ok(fd)
    }

    pub fn mkdir(&self, path: &str) -> Result<(), &'static str> {
        let mut vfs = self.inner.lock();
        let resolved = vfs.resolve_path(path);
        let (fs, remaining) = vfs.find_fs(&resolved).ok_or("not found")?;
        fs.mkdir(&remaining)
    }

    pub fn unlink(&self, path: &str) -> Result<(), &'static str> {
        let mut vfs = self.inner.lock();
        let resolved = vfs.resolve_path(path);
        let (fs, remaining) = vfs.find_fs(&resolved).ok_or("not found")?;
        fs.unlink(&remaining)
    }

    pub fn readdir(&self, path: &str) -> Result<Vec<VNode>, &'static str> {
        self.inner.lock().readdir(path)
    }

    pub fn exists(&self, path: &str) -> bool {
        self.inner.lock().exists(path)
    }
}

// ── Global VFS context singleton (backward-compatible) ─────────────

static VFS_CTX: Mutex<Option<VfsContext>> = Mutex::new(None);

/// Initialise the global VFS context.
pub fn init_vfs() {
    *VFS_CTX.lock() = Some(VfsContext::new());
    log::info!("VFS: mounted MemFileSystem at /");
}

/// Get the global VFS context.
pub fn get_vfs() -> &'static Mutex<Option<VfsContext>> {
    &VFS_CTX
}

/// Execute a closure over the VfsContext (immutable access via &self).
pub fn with_vfs<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&VfsContext) -> R,
{
    VFS_CTX.lock().as_ref().map(f)
}

/// Execute a mutable closure (though VfsContext uses interior mutability,
/// this exists for symmetry with other context types).
pub fn with_vfs_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&VfsContext) -> R,
{
    VFS_CTX.lock().as_ref().map(f)
}

// ── Backward-compatible free functions ────────────────────────────

/// Backward-compatible wrapper: mount a tmpfs.
pub fn mount(device: &str, mount_point: &str, fs_type: &str) -> Result<(), &'static str> {
    with_vfs(|vfs| match fs_type {
        "tmpfs" => {
            let memfs = Box::new(MemFileSystem::new());
            vfs.mount(mount_point, memfs)?;
            log::info!("VFS: mounted tmpfs from {} at {}", device, mount_point);
            Ok(())
        }
        _ => Err("unsupported filesystem type"),
    })
    .ok_or("vfs not init")?
}

/// Backward-compatible wrapper: open a file.
pub fn open(path: &str, flags: u32) -> Result<FileDescriptor, &'static str> {
    with_vfs(|vfs| vfs.open(path, flags))
        .ok_or("vfs not init")?
        .ok_or("not found")
}

/// Backward-compatible wrapper: read from fd.
pub fn read(fd: u32, buf: &mut [u8]) -> Result<usize, &'static str> {
    with_vfs(|vfs| vfs.read(fd, buf)).ok_or("vfs not init")?
}

/// Backward-compatible wrapper: write to fd.
pub fn write(fd: u32, data: &[u8]) -> Result<usize, &'static str> {
    with_vfs(|vfs| vfs.write(fd, data)).ok_or("vfs not init")?
}

/// Backward-compatible wrapper: close fd.
pub fn close(fd: u32) -> Result<(), &'static str> {
    with_vfs(|vfs| vfs.close(fd)).ok_or("vfs not init")?
}

/// Backward-compatible wrapper: seek fd.
pub fn seek(fd: u32, pos: usize) -> Result<(), &'static str> {
    with_vfs(|vfs| vfs.seek(fd, pos)).ok_or("vfs not init")?
}

/// Backward-compatible wrapper: readdir.
pub fn readdir(path: &str) -> Result<Vec<VNode>, &'static str> {
    with_vfs(|vfs| vfs.readdir(path)).ok_or("vfs not init")?
}

/// Backward-compatible wrapper: create file.
pub fn create(path: &str) -> Result<FileDescriptor, &'static str> {
    with_vfs(|vfs| vfs.create(path)).ok_or("vfs not init")?
}

/// Backward-compatible wrapper: mkdir.
pub fn mkdir(path: &str) -> Result<(), &'static str> {
    with_vfs(|vfs| vfs.mkdir(path)).ok_or("vfs not init")?
}

/// Backward-compatible wrapper: unlink.
pub fn unlink(path: &str) -> Result<(), &'static str> {
    with_vfs(|vfs| vfs.unlink(path)).ok_or("vfs not init")?
}

/// Backward-compatible wrapper: exists.
pub fn exists(path: &str) -> bool {
    with_vfs(|vfs| vfs.exists(path)).unwrap_or(false)
}

/// Backward-compatible wrapper: working directory.
pub fn working_directory() -> Result<String, &'static str> {
    with_vfs(|vfs| vfs.working_directory()).ok_or("vfs not init")
}

/// Backward-compatible wrapper: change directory.
pub fn change_directory(path: &str) -> Result<(), &'static str> {
    with_vfs(|vfs| vfs.change_directory(path)).ok_or("vfs not init")?
}
