//! VfsContext — unified VFS state that replaces the scattered `vfs::open()` /
//! `vfs::read()` / `vfs::cwd` / `vfs::mount` global calls.
//!
//! Holds mount table, working directory, and open-handle routing inside a single
//! context struct so that higher layers access the VFS through
//! `kernel.vfs.open(path)` rather than scattered module-level statics.
//!
//! # Locking
//!
//! `VfsContext` uses internal `spin::Mutex` for `inner` (Vfs dispatcher) and
//! `handle_table`.  Lock ordering is consistently **inner → handle_table**
//! (or handle_table alone, never handle_table → inner) to prevent deadlocks.
//!
//! `VfsContext` lives inside `KernelContext` which is itself behind a
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
/// kernel_context().with_kernel(|k| {
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

    /// Find and remove an entry in a single lock acquisition to avoid
    /// double-lock on `handle_table`.
    fn take(&mut self, fd: u32) -> Option<usize> {
        if let Some(pos) = self.entries.iter().position(|e| e.fd == fd) {
            let entry = self.entries.remove(pos);
            Some(entry.mount_index)
        } else {
            None
        }
    }
}

impl VfsContext {
    /// Create a new VFS context with a memory-backed root filesystem.
    pub fn new() -> Self {
        let root_fs = Box::new(MemFileSystem::new());
        Self {
            inner: Mutex::new(Vfs::new(root_fs)),
            handle_table: Mutex::new(HandleTable::new()),
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
    //
    // Lock ordering rule: always acquire `inner` before `handle_table`,
    // never the reverse.  This prevents deadlock with interrupt- or
    // multi-context callers that may interleave operations.

    pub fn open(&self, path: &str, flags: u32) -> Option<FileDescriptor> {
        // Acquire inner first, do all FS work, drop inner…
        let (mount_index, fd) = {
            let mut vfs = self.inner.lock();
            let mount_index = vfs.find_fs_index(path)?;
            let fd = vfs.open(path, flags)?;
            (mount_index, fd)
        };
        // …then acquire handle_table.
        self.handle_table.lock().insert(fd.fd, mount_index);
        Some(fd)
    }

    pub fn read(&self, fd: u32, buf: &mut [u8]) -> Result<usize, &'static str> {
        // Acquire inner first, then handle_table (correct lock order).
        let mut vfs = self.inner.lock();
        let mount_idx = self
            .handle_table
            .lock()
            .find(fd)
            .ok_or("bad fd")?;
        vfs.read_at(mount_idx, fd, buf)
    }

    pub fn write(&self, fd: u32, data: &[u8]) -> Result<usize, &'static str> {
        // Acquire inner first, then handle_table (correct lock order).
    pub fn write(&self, fd: u32, data: &[u8]) -> Result<usize, &'static str> {
        let mut vfs = self.inner.lock();
        let mount_idx = self
            .handle_table
            .lock()
            .find(fd)
            .ok_or("bad fd")?;
        vfs.write_at(mount_idx, fd, data)
    }

    pub fn close(&self, fd: u32) -> Result<(), &'static str> {
        // Acquire inner first, then handle_table (correct lock order).
    pub fn close(&self, fd: u32) -> Result<(), &'static str> {
        let mut vfs = self.inner.lock();
        let mount_idx = self
            .handle_table
            .lock()
            .take(fd)
            .ok_or("bad fd")?;
        vfs.close_at(mount_idx, fd)
    }

    pub fn seek(&self, fd: u32, pos: usize) -> Result<(), &'static str> {
        // Acquire inner first, then handle_table (correct lock order).
        let mut vfs = self.inner.lock();
        let mount_idx = self
            .handle_table
            .lock()
            .find(fd)
            .ok_or("bad fd")?;
        vfs.seek_at(mount_idx, fd, pos)
    }

    pub fn create(&self, path: &str) -> Result<FileDescriptor, &'static str> {
        // Acquire inner first, do all FS work, drop inner…
        let (mount_index, fd) = {
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
            (mount_index, fd)
        };
        // …then acquire handle_table.
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

// ── Global VFS context singleton ────────────────────────────────────
//
// NOTE: The canonical VfsContext lives inside `KernelContext.vfs`.
// This static exists ONLY to provide a global accessor that routes
// through KernelContext.  It is NOT a separate VfsContext instance.
// The `init_vfs()` function is called once during boot and ensures
// the VfsContext inside KernelContext is properly initialised.
//
// The free functions below (`open`, `read`, `close`, …) delegate
// through `with_kernel()` → `kernel.vfs.*` to guarantee that there
// is exactly ONE VfsContext in the system.

/// Initialise the global VFS context.
///
/// This is idempotent: if KernelContext already has a VfsContext with
/// a root filesystem mounted, this is a no-op.
pub fn init_vfs() {
    // VfsContext is already created inside KernelContext::new().
    // This function exists for the bootstrap sequence and backward
    // compatibility.  Additional per-fs init (e.g. creating /bootlogs)
    // is handled by the caller.
    log::info!("VFS: mounted MemFileSystem at /");
}

/// Execute a closure over the VfsContext.
///
/// Routes through `KernelContext.vfs` to guarantee single-instance.
pub fn with_vfs<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&VfsContext) -> R,
{
    super::kernel::with_kernel(|k| f(&k.vfs))
}

/// Execute a mutable closure (though VfsContext uses interior mutability,
/// this exists for symmetry with other context types).
pub fn with_vfs_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&VfsContext) -> R,
{
    super::kernel::with_kernel(|k| f(&k.vfs))
}

// ── Backward-compatible free functions ────────────────────────────
//
// All delegate through `with_vfs` → `KernelContext.vfs`, guaranteeing
// single VfsContext instance.

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

// ── Panic-safe VFS access (for klog flush) ─────────────────────────

/// Check whether the VFS system is accessible without blocking.
///
/// Used by `flush_to_vfs_safe()` in the panic handler.  Returns `true`
/// if the KernelContext, the inner VFS dispatcher, and the handle_table
/// can all be locked without blocking.
///
/// Note: `spin::Mutex` does not have poisoning semantics
/// (`std::sync::Mutex`), so a successful `try_lock()` followed by a
/// drop-and-reacquire (in `flush_to_vfs`) is safe in a panic handler.
pub fn vfs_try_accessible() -> bool {
    // Try to lock KernelContext first.
    let kernel_guard = super::kernel::get_kernel().try_lock();
    let Some(kernel_guard) = kernel_guard else {
        return false;
    };
    let Some(kernel) = kernel_guard.as_ref() else {
        return false;
    };
    // Now try the inner VFS lock and handle_table lock while we hold the kernel guard.
    let inner_ok = kernel.vfs.inner.try_lock().is_some();
    let handle_table_ok = kernel.vfs.handle_table.try_lock().is_some();
    // Drop the kernel guard implicitly.
    inner_ok && handle_table_ok
}
