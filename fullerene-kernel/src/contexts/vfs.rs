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

    pub fn mount(&self, mount_point: &str, fs: Box<dyn FileSystem>) -> Result<(), &'static str> {
        self.inner.lock().mount(mount_point, fs)
    }

    /// Unmount a filesystem at `mount_point`.
    ///
    /// Returns `Ok(true)` if a mount was removed, `Ok(false)` if none existed.
    /// The root `"/"` cannot be unmounted.
    ///
    /// Also updates the handle table: open file descriptors belonging to the
    /// unmounted filesystem are discarded, and indices of remaining mounts
    /// that shifted left are decremented.
    pub fn unmount(&self, mount_point: &str) -> Result<bool, &'static str> {
        let mut vfs = self.inner.lock();
        let mut handle_table = self.handle_table.lock();
        let target_idx = match vfs.find_fs_index(mount_point) {
            Some(idx) => idx,
            None => return Ok(false),
        };
        let removed = vfs.unmount(mount_point)?;
        if removed {
            handle_table
                .entries
                .retain(|entry| entry.mount_index != target_idx);
            for entry in &mut handle_table.entries {
                if entry.mount_index > target_idx {
                    entry.mount_index -= 1;
                }
            }
        }
        Ok(removed)
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
        let mount_idx = self.handle_table.lock().find(fd).ok_or("bad fd")?;
        vfs.read_at(mount_idx, fd, buf)
    }

    pub fn write(&self, fd: u32, data: &[u8]) -> Result<usize, &'static str> {
        let mut vfs = self.inner.lock();
        let mount_idx = self.handle_table.lock().find(fd).ok_or("bad fd")?;
        vfs.write_at(mount_idx, fd, data)
    }

    pub fn close(&self, fd: u32) -> Result<(), &'static str> {
        let mut vfs = self.inner.lock();
        let mount_idx = self.handle_table.lock().take(fd).ok_or("bad fd")?;
        vfs.close_at(mount_idx, fd)
    }

    pub fn seek(&self, fd: u32, pos: usize) -> Result<(), &'static str> {
        let mut vfs = self.inner.lock();
        let mount_idx = self.handle_table.lock().find(fd).ok_or("bad fd")?;
        vfs.seek_at(mount_idx, fd, pos)
    }

    pub fn create(&self, path: &str) -> Result<FileDescriptor, &'static str> {
        // Acquire inner first, do all FS work, drop inner…
        let (mount_index, fd) = {
            let mut vfs = self.inner.lock();
            let mount_index = vfs.find_fs_index(path).ok_or("not found")?;
            let resolved = vfs.resolve_path(path);
            {
                let (fs, remaining) = vfs.find_fs(&resolved).ok_or("not found")?;
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

/// Backward-compatible wrapper: mount a filesystem.
///
/// Supported `fs_type` values:
/// - `"tmpfs"` — mounts a fresh in-memory filesystem ([`MemFileSystem`]).
/// - `"fat32"` — mounts a FAT32/exFAT filesystem backed by a block device.
///
/// The `device` argument is the path to a block device that the kernel
/// can open.  For `tmpfs`, `device` is ignored.
pub fn mount(device: &str, mount_point: &str, fs_type: &str) -> Result<(), &'static str> {
    with_vfs(|vfs| match fs_type {
        "tmpfs" => {
            let memfs = Box::new(MemFileSystem::new());
            vfs.mount(mount_point, memfs)?;
            log::info!("VFS: mounted tmpfs from {} at {}", device, mount_point);
            Ok(())
        }
        "fat32" => {
            // The kernel's USB storage driver already mounts FatFileSystem
            // directly via `vfs.mount(mount_point, Box::new(fs))` when a
            // disk is detected.  This code path is a convenience for
            // mounting a known block device by name (e.g. `/dev/sda`) at
            // boot.  We look up the device in the kernel's driver registry.
            let fs = crate::drivers::fat::open_block_device(device)?;
            vfs.mount(mount_point, Box::new(fs))?;
            log::info!("VFS: mounted fat32 from {} at {}", device, mount_point);
            Ok(())
        }
        _ => Err("unsupported filesystem type"),
    })
    .ok_or("vfs not init")?
}

/// Backward-compatible wrapper: unmount a filesystem at `mount_point`.
pub fn unmount(mount_point: &str) -> Result<bool, &'static str> {
    with_vfs(|vfs| vfs.unmount(mount_point)).ok_or("vfs not init")?
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

/// RAII guard proving that all VFS-related locks were successfully
/// acquired via `try_lock`.  Holding this guard guarantees that no
/// other thread holds the kernel, VFS inner, or handle_table locks,
/// so a subsequent blocking `flush_to_vfs()` call cannot deadlock.
///
/// `spin::Mutex` does not have poisoning semantics, so re-acquiring
/// the same locks after dropping this guard is safe in a panic handler.
pub struct VfsAccessGuard {
    _kernel: spin::MutexGuard<'static, Option<super::kernel::KernelContext>>,
    _inner: spin::MutexGuard<'static, crate::vfs::Vfs>,
    _handle_table: spin::MutexGuard<'static, super::vfs::HandleTable>,
}

// SAFETY: VfsAccessGuard is !Send + !Sync by construction (it holds
// MutexGuard which is !Send).  This is intentional — the guard must
// not be held across await points or thread boundaries.
// No manual impl needed; the auto-derived negative impls are correct.

/// Try to acquire all VFS-related locks without blocking.
///
/// Returns `Some(guard)` if the kernel context, VFS dispatcher, and
/// handle table were all successfully locked via `try_lock`.
/// Returns `None` if any lock is currently held by another thread
/// (or by the panicking thread itself in a re-entrant scenario).
///
/// The returned [`VfsAccessGuard`] proves that the coast is clear;
/// the caller can drop it and immediately call `flush_to_vfs()` with
/// confidence that the locks are free (in a single-threaded kernel,
/// no other thread can steal them between drop and re-acquire).
pub fn vfs_try_access() -> Option<VfsAccessGuard> {
    // SAFETY: kernel_guard borrows from a global static that lives for
    // the entire program.  Transmute to 'static before accessing inner
    // fields to avoid borrow conflicts during the move into VfsAccessGuard.
    let kernel_guard = super::kernel::get_kernel().try_lock()?;
    let kernel_guard: spin::MutexGuard<'static, Option<super::kernel::KernelContext>> =
        unsafe { core::mem::transmute(kernel_guard) };
    let kernel = kernel_guard.as_ref()?;

    // Acquire inner and handle_table while holding the kernel guard
    // to preserve lock ordering (kernel → inner → handle_table).
    let inner_guard = kernel.vfs.inner.try_lock()?;
    let handle_table_guard = kernel.vfs.handle_table.try_lock()?;

    // SAFETY: inner_guard and handle_table_guard also borrow from global
    // statics inside KernelContext.vfs, which lives forever.
    let inner_guard: spin::MutexGuard<'static, crate::vfs::Vfs> =
        unsafe { core::mem::transmute(inner_guard) };
    let handle_table_guard: spin::MutexGuard<'static, super::vfs::HandleTable> =
        unsafe { core::mem::transmute(handle_table_guard) };

    Some(VfsAccessGuard {
        _kernel: kernel_guard,
        _inner: inner_guard,
        _handle_table: handle_table_guard,
    })
}
