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

use genome::fs::FsError;
pub use genome::vfs::{
    FileDescriptor, FileSystem, FileSystemCapabilities, InodeType, MemFileSystem, VNode, Vfs,
};

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

    /// Open-handle tracking so that `read(fd)` / `close(fd)` routes a
    /// VFS-global descriptor to the owning mount and its local descriptor.
    handle_table: Mutex<HandleTable>,

    /// Block-device source and its mount point. Genome remains generic while
    /// device-facing kernel code can expose only reachable storage.
    mounted_devices: Mutex<Vec<(String, String)>>,
}

/// Tracks which mount owns each VFS-global file descriptor.
struct HandleTable {
    entries: Vec<HandleEntry>,
    next_fd: u32,
}

struct HandleEntry {
    fd: u32,
    mount_index: usize,
    local_fd: u32,
}

#[derive(Clone, Copy)]
struct HandleLocation {
    mount_index: usize,
    local_fd: u32,
}

impl HandleTable {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_fd: 0,
        }
    }

    fn insert(&mut self, descriptor: FileDescriptor, mount_index: usize) -> Option<FileDescriptor> {
        let fd = self.allocate_fd()?;
        let local_fd = descriptor.fd;
        self.entries.push(HandleEntry {
            fd,
            mount_index,
            local_fd,
        });
        Some(FileDescriptor { fd, ..descriptor })
    }

    fn find(&self, fd: u32) -> Option<HandleLocation> {
        self.entries
            .iter()
            .find(|e| e.fd == fd)
            .map(|e| HandleLocation {
                mount_index: e.mount_index,
                local_fd: e.local_fd,
            })
    }

    /// Find and remove an entry in a single lock acquisition to avoid
    /// double-lock on `handle_table`.
    fn take(&mut self, fd: u32) -> Option<HandleLocation> {
        if let Some(pos) = self.entries.iter().position(|e| e.fd == fd) {
            let entry = self.entries.remove(pos);
            Some(HandleLocation {
                mount_index: entry.mount_index,
                local_fd: entry.local_fd,
            })
        } else {
            None
        }
    }

    fn allocate_fd(&mut self) -> Option<u32> {
        let start = self.next_fd;
        loop {
            let candidate = self.next_fd;
            self.next_fd = self.next_fd.wrapping_add(1);
            if self.entries.iter().all(|entry| entry.fd != candidate) {
                return Some(candidate);
            }
            if self.next_fd == start {
                return None;
            }
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
            mounted_devices: Mutex::new(Vec::new()),
        }
    }

    // ── Working directory ───────────────────────────────────────

    pub fn working_directory(&self) -> String {
        let vfs = self.inner.lock();
        String::from(vfs.working_directory())
    }

    pub fn change_directory(&self, path: &str) -> Result<(), FsError> {
        self.inner.lock().change_directory(path)
    }

    // ── Mount management ────────────────────────────────────────

    pub fn mount(&self, mount_point: &str, fs: Box<dyn FileSystem>) -> Result<(), FsError> {
        let mut vfs = self.inner.lock();
        if vfs.mounted_fs_index(mount_point).is_some() {
            return Err(FsError::FileExists);
        }
        vfs.mount(mount_point, fs)?;
        Ok(())
    }

    /// Unmount a filesystem at `mount_point`.
    ///
    /// Returns `Ok(true)` if a mount was removed, `Ok(false)` if none existed.
    /// The root `"/"` cannot be unmounted.
    ///
    /// Also updates the handle table: open file descriptors belonging to the
    /// unmounted filesystem are discarded, and indices of remaining mounts
    /// that shifted left are decremented.
    pub fn unmount(&self, mount_point: &str) -> Result<bool, FsError> {
        let mount_point = self.inner.lock().resolve_path(mount_point);
        let removed = {
            let mut vfs = self.inner.lock();
            let mut handle_table = self.handle_table.lock();
            let target_idx = match vfs.mounted_fs_index(&mount_point) {
                Some(idx) => idx,
                None => return Ok(false),
            };
            let removed = vfs.unmount(&mount_point)?;
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
            removed
        };
        if removed {
            let returned_devices: Vec<String> = {
                let mut mounted_devices = self.mounted_devices.lock();
                let returned = mounted_devices
                    .iter()
                    .filter(|(_, path)| path == &mount_point)
                    .map(|(source, _)| source.clone())
                    .collect();
                mounted_devices.retain(|(_, path)| path != &mount_point);
                returned
            };
            for source in returned_devices {
                if let Some(device_name) = source.strip_prefix("/dev/") {
                    crate::devfs::return_block_device_lease(device_name);
                }
            }
        }
        Ok(removed)
    }

    fn record_device_mount(&self, device: &str, mount_point: &str) {
        let mut mounts = self.mounted_devices.lock();
        mounts.retain(|(source, path)| source != device && path != mount_point);
        mounts.push((String::from(device), String::from(mount_point)));
    }

    /// Return block devices which are currently reachable through the VFS.
    pub fn mounted_block_devices(&self) -> Vec<(String, String)> {
        self.mounted_devices.lock().clone()
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
        self.register_handle(mount_index, fd)
    }

    pub fn read(&self, fd: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        // Acquire inner first, then handle_table (correct lock order).
        let mut vfs = self.inner.lock();
        let handle = self
            .handle_table
            .lock()
            .find(fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
        vfs.read_at(handle.mount_index, handle.local_fd, buf)
    }

    pub fn write(&self, fd: u32, data: &[u8]) -> Result<usize, FsError> {
        let mut vfs = self.inner.lock();
        let handle = self
            .handle_table
            .lock()
            .find(fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
        vfs.write_at(handle.mount_index, handle.local_fd, data)
    }

    pub fn close(&self, fd: u32) -> Result<(), FsError> {
        let mut vfs = self.inner.lock();
        let handle = self
            .handle_table
            .lock()
            .take(fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
        vfs.close_at(handle.mount_index, handle.local_fd)
    }

    pub fn seek(&self, fd: u32, pos: u64) -> Result<(), FsError> {
        let mut vfs = self.inner.lock();
        let handle = self
            .handle_table
            .lock()
            .find(fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
        vfs.seek_at(handle.mount_index, handle.local_fd, pos)
    }

    pub fn create(&self, path: &str) -> Result<FileDescriptor, FsError> {
        // Acquire inner first, do all FS work, drop inner…
        let (mount_index, fd) = {
            let mut vfs = self.inner.lock();
            let mount_index = vfs.find_fs_index(path).ok_or(FsError::FileNotFound)?;
            let resolved = vfs.resolve_path(path);
            {
                let (fs, remaining) = vfs.find_fs(&resolved).ok_or(FsError::FileNotFound)?;
                if !fs.exists(&remaining) {
                    fs.create(&remaining, InodeType::File)
                        .ok_or(FsError::PermissionDenied)?;
                }
            }
            let (fs, remaining) = vfs.find_fs(&resolved).ok_or(FsError::FileNotFound)?;
            let fd = fs.open(&remaining, 0).ok_or(FsError::FileNotFound)?;
            (mount_index, fd)
        };
        // …then acquire handle_table.
        self.register_handle(mount_index, fd)
            .ok_or(FsError::PermissionDenied)
    }

    pub fn mkdir(&self, path: &str) -> Result<(), FsError> {
        let mut vfs = self.inner.lock();
        let resolved = vfs.resolve_path(path);
        let (fs, remaining) = vfs.find_fs(&resolved).ok_or(FsError::FileNotFound)?;
        fs.mkdir(&remaining)
    }

    pub fn unlink(&self, path: &str) -> Result<(), FsError> {
        let mut vfs = self.inner.lock();
        let resolved = vfs.resolve_path(path);
        let (fs, remaining) = vfs.find_fs(&resolved).ok_or(FsError::FileNotFound)?;
        fs.unlink(&remaining)
    }

    pub fn readdir(&self, path: &str) -> Result<Vec<VNode>, FsError> {
        self.inner.lock().readdir(path)
    }

    pub fn exists(&self, path: &str) -> bool {
        self.inner.lock().exists(path)
    }

    /// Replace a regular file and persist the complete buffer, even when the
    /// backing filesystem accepts only a partial write per call.
    pub fn replace_file(&self, path: &str, data: &[u8]) -> Result<(), FsError> {
        if self.exists(path) {
            self.unlink(path)?;
        }
        let descriptor = self.create(path)?;
        let result = self.write_all(descriptor.fd, data);
        let close_result = self.close(descriptor.fd);
        result.and(close_result)
    }

    fn write_all(&self, fd: u32, mut data: &[u8]) -> Result<(), FsError> {
        while !data.is_empty() {
            let written = self.write(fd, data)?;
            if written == 0 {
                return Err(FsError::InvalidInput);
            }
            data = &data[written..];
        }
        Ok(())
    }

    /// Copy a file or directory tree without buffering whole files in the GUI.
    pub fn copy_path(&self, source: &str, destination: &str, is_dir: bool) -> Result<(), FsError> {
        let destination_suffix = destination.strip_prefix(source);
        if source == destination
            || is_dir && destination_suffix.is_some_and(|suffix| suffix.starts_with('/'))
        {
            return Err(FsError::InvalidPath);
        }
        if is_dir {
            self.mkdir(destination)?;
            for entry in self.readdir(source)? {
                self.copy_path(
                    &join_path(source, &entry.name),
                    &join_path(destination, &entry.name),
                    entry.is_dir,
                )?;
            }
            return Ok(());
        }

        let source_fd = self.open(source, 0).ok_or(FsError::FileNotFound)?;
        if self.exists(destination) {
            if let Err(error) = self.unlink(destination) {
                let _ = self.close(source_fd.fd);
                return Err(error);
            }
        }
        let destination_fd = match self.create(destination) {
            Ok(descriptor) => descriptor,
            Err(error) => {
                let _ = self.close(source_fd.fd);
                return Err(error);
            }
        };
        let result = (|| {
            let mut buffer = [0; 4096];
            loop {
                let read = self.read(source_fd.fd, &mut buffer)?;
                if read == 0 {
                    return Ok(());
                }
                self.write_all(destination_fd.fd, &buffer[..read])?;
            }
        })();
        let source_close = self.close(source_fd.fd);
        let destination_close = self.close(destination_fd.fd);
        result.and(source_close).and(destination_close)
    }

    /// Recursively remove a path selected by the file manager.
    pub fn remove_path(&self, path: &str, is_dir: bool) -> Result<(), FsError> {
        if is_dir {
            for entry in self.readdir(path)? {
                self.remove_path(&join_path(path, &entry.name), entry.is_dir)?;
            }
        }
        self.unlink(path)
    }

    /// Move within or across mounts while preserving the source until copy succeeds.
    pub fn move_path(&self, source: &str, destination: &str, is_dir: bool) -> Result<(), FsError> {
        self.copy_path(source, destination, is_dir)?;
        self.remove_path(source, is_dir)
    }

    fn register_handle(
        &self,
        mount_index: usize,
        descriptor: FileDescriptor,
    ) -> Option<FileDescriptor> {
        let local_fd = descriptor.fd;
        let registered = { self.handle_table.lock().insert(descriptor, mount_index) };
        if registered.is_none() {
            let _ = self.inner.lock().close_at(mount_index, local_fd);
        }
        registered
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
    // compatibility. Additional per-filesystem initialization
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
/// - `"devfs"` — mounts the kernel's dynamic device filesystem.
/// - `"auto"` — detects and mounts a FAT12/16/32 or exFAT block device.
/// - `"fat32"` — retained as a backward-compatible alias for `"auto"`.
///
/// The `device` argument is the path to a block device that the kernel
/// can open.  For `tmpfs`, `device` is ignored.
pub fn mount(device: &str, mount_point: &str, fs_type: &str) -> Result<(), FsError> {
    with_vfs(|vfs| match fs_type {
        "tmpfs" => {
            let memfs = Box::new(MemFileSystem::new());
            vfs.mount(mount_point, memfs)?;
            log::info!("VFS: mounted tmpfs from {} at {}", device, mount_point);
            Ok(())
        }
        "devfs" => {
            vfs.mount(mount_point, Box::new(crate::devfs::DevFs::new()))?;
            log::info!("VFS: mounted devfs at {}", mount_point);
            Ok(())
        }
        "auto" | "fat32" => {
            let mount_point = vfs.inner.lock().resolve_path(mount_point);
            let device_name = device
                .strip_prefix("/dev/")
                .filter(|name| !name.is_empty() && !name.contains('/'))
                .ok_or(FsError::InvalidPath)?;

            if !crate::devfs::block_device_exists(device_name) {
                return Err(FsError::FileNotFound);
            }
            if vfs.inner.lock().mounted_fs_index(&mount_point).is_some() {
                return Err(FsError::FileExists);
            }

            // Validate mount point before consuming the block device
            if !vfs.exists(&mount_point) {
                vfs.mkdir(&mount_point)?;
            }

            let bdev =
                crate::devfs::lease_block_device(device_name).ok_or(FsError::PermissionDenied)?;
            match crate::drivers::fat::mount_device(bdev) {
                Ok(fs) => {
                    if let Err(e) = vfs.mount(&mount_point, fs) {
                        crate::devfs::return_block_device_lease(device_name);
                        return Err(e);
                    }
                    vfs.record_device_mount(device, &mount_point);
                    log::info!(
                        "VFS: mounted removable filesystem from {} at {}",
                        device,
                        mount_point
                    );
                    Ok(())
                }
                Err((e, returned_bdev)) => {
                    drop(returned_bdev);
                    crate::devfs::return_block_device_lease(device_name);
                    Err(e)
                }
            }
        }
        _ => Err(FsError::NotSupported),
    })
    .ok_or(FsError::PermissionDenied)?
}

/// Backward-compatible wrapper: unmount a filesystem at `mount_point`.
pub fn unmount(mount_point: &str) -> Result<bool, FsError> {
    with_vfs(|vfs| vfs.unmount(mount_point)).ok_or(FsError::PermissionDenied)?
}

/// Backward-compatible wrapper: open a file.
pub fn open(path: &str, flags: u32) -> Result<FileDescriptor, FsError> {
    with_vfs(|vfs| vfs.open(path, flags))
        .ok_or(FsError::PermissionDenied)?
        .ok_or(FsError::FileNotFound)
}

/// Backward-compatible wrapper: read from fd.
pub fn read(fd: u32, buf: &mut [u8]) -> Result<usize, FsError> {
    with_vfs(|vfs| vfs.read(fd, buf)).ok_or(FsError::PermissionDenied)?
}

/// Backward-compatible wrapper: write to fd.
pub fn write(fd: u32, data: &[u8]) -> Result<usize, FsError> {
    with_vfs(|vfs| vfs.write(fd, data)).ok_or(FsError::PermissionDenied)?
}

/// Backward-compatible wrapper: close fd.
pub fn close(fd: u32) -> Result<(), FsError> {
    with_vfs(|vfs| vfs.close(fd)).ok_or(FsError::PermissionDenied)?
}

/// Backward-compatible wrapper: seek fd.
pub fn seek(fd: u32, pos: u64) -> Result<(), FsError> {
    with_vfs(|vfs| vfs.seek(fd, pos)).ok_or(FsError::PermissionDenied)?
}

/// Backward-compatible wrapper: readdir.
pub fn readdir(path: &str) -> Result<Vec<VNode>, FsError> {
    with_vfs(|vfs| vfs.readdir(path)).ok_or(FsError::PermissionDenied)?
}

/// Backward-compatible wrapper: create file.
pub fn create(path: &str) -> Result<FileDescriptor, FsError> {
    with_vfs(|vfs| vfs.create(path)).ok_or(FsError::PermissionDenied)?
}

/// Backward-compatible wrapper: mkdir.
pub fn mkdir(path: &str) -> Result<(), FsError> {
    with_vfs(|vfs| vfs.mkdir(path)).ok_or(FsError::PermissionDenied)?
}

/// Backward-compatible wrapper: unlink.
pub fn unlink(path: &str) -> Result<(), FsError> {
    with_vfs(|vfs| vfs.unlink(path)).ok_or(FsError::PermissionDenied)?
}

/// Backward-compatible wrapper: exists.
pub fn exists(path: &str) -> bool {
    with_vfs(|vfs| vfs.exists(path)).unwrap_or(false)
}

/// Replace a complete file through the canonical VFS context.
pub fn replace_file(path: &str, data: &[u8]) -> Result<(), FsError> {
    with_vfs(|vfs| vfs.replace_file(path, data)).ok_or(FsError::PermissionDenied)?
}

pub fn copy_path(source: &str, destination: &str, is_dir: bool) -> Result<(), FsError> {
    with_vfs(|vfs| vfs.copy_path(source, destination, is_dir)).ok_or(FsError::PermissionDenied)?
}

pub fn move_path(source: &str, destination: &str, is_dir: bool) -> Result<(), FsError> {
    with_vfs(|vfs| vfs.move_path(source, destination, is_dir)).ok_or(FsError::PermissionDenied)?
}

pub fn remove_path(path: &str, is_dir: bool) -> Result<(), FsError> {
    with_vfs(|vfs| vfs.remove_path(path, is_dir)).ok_or(FsError::PermissionDenied)?
}

fn join_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        alloc::format!("/{}", name)
    } else {
        alloc::format!("{}/{}", parent.trim_end_matches('/'), name)
    }
}

/// Backward-compatible wrapper: working directory.
pub fn working_directory() -> Result<String, FsError> {
    with_vfs(|vfs| vfs.working_directory()).ok_or(FsError::PermissionDenied)
}

/// Backward-compatible wrapper: change directory.
pub fn change_directory(path: &str) -> Result<(), FsError> {
    with_vfs(|vfs| vfs.change_directory(path)).ok_or(FsError::PermissionDenied)?
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
    _inner: spin::MutexGuard<'static, Vfs>,
    _handle_table: spin::MutexGuard<'static, HandleTable>,
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
    let inner_guard: spin::MutexGuard<'static, Vfs> = unsafe { core::mem::transmute(inner_guard) };
    let handle_table_guard: spin::MutexGuard<'static, HandleTable> =
        unsafe { core::mem::transmute(handle_table_guard) };

    Some(VfsAccessGuard {
        _kernel: kernel_guard,
        _inner: inner_guard,
        _handle_table: handle_table_guard,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_table_assigns_unique_descriptors_to_local_fd_collisions() {
        let mut table = HandleTable::new();
        let local_descriptor = FileDescriptor {
            fd: 0,
            ino: 1,
            offset: 0,
            flags: 0,
        };

        let first = table.insert(local_descriptor.clone(), 0).unwrap();
        let second = table.insert(local_descriptor, 1).unwrap();

        assert_ne!(first.fd, second.fd);
        let first_location = table.find(first.fd).unwrap();
        let second_location = table.find(second.fd).unwrap();
        assert_eq!(first_location.mount_index, 0);
        assert_eq!(first_location.local_fd, 0);
        assert_eq!(second_location.mount_index, 1);
        assert_eq!(second_location.local_fd, 0);
    }

    #[test]
    fn vfs_context_routes_colliding_local_descriptors_to_their_mounts() {
        let context = VfsContext::new();
        context.mkdir("/mnt").unwrap();
        context
            .mount("/mnt", Box::new(MemFileSystem::new()))
            .unwrap();

        let root_file = context.create("/root-file").unwrap();
        let mounted_file = context.create("/mnt/mounted-file").unwrap();
        assert_ne!(root_file.fd, mounted_file.fd);

        context.write(root_file.fd, b"root").unwrap();
        context.write(mounted_file.fd, b"mounted").unwrap();
        context.seek(root_file.fd, 0).unwrap();
        context.seek(mounted_file.fd, 0).unwrap();

        let mut root_data = [0; 4];
        let mut mounted_data = [0; 7];
        context.read(root_file.fd, &mut root_data).unwrap();
        context.read(mounted_file.fd, &mut mounted_data).unwrap();
        assert_eq!(&root_data, b"root");
        assert_eq!(&mounted_data, b"mounted");
    }

    #[test]
    fn unmount_removes_device_mount_metadata() {
        let context = VfsContext::new();
        context.mkdir("/mnt").unwrap();
        context
            .mount("/mnt", Box::new(MemFileSystem::new()))
            .unwrap();
        context.record_device_mount("/dev/sd0", "/mnt");

        assert_eq!(
            context.mounted_block_devices(),
            vec![(String::from("/dev/sd0"), String::from("/mnt"))]
        );
        assert!(context.unmount("/mnt").unwrap());
        assert!(context.mounted_block_devices().is_empty());
    }

    #[test]
    fn copy_path_streams_complete_files_across_mounts() {
        let context = VfsContext::new();
        context.mkdir("/mnt").unwrap();
        context
            .mount("/mnt", Box::new(MemFileSystem::new()))
            .unwrap();
        let expected: Vec<u8> = (0..9_000).map(|index| (index % 251) as u8).collect();

        context.replace_file("/source.bin", &expected).unwrap();
        context
            .copy_path("/source.bin", "/mnt/copied.bin", false)
            .unwrap();

        let descriptor = context.open("/mnt/copied.bin", 0).unwrap();
        let mut actual = vec![0; expected.len()];
        let mut offset = 0;
        while offset < actual.len() {
            let read = context.read(descriptor.fd, &mut actual[offset..]).unwrap();
            if read == 0 {
                break;
            }
            offset += read;
        }
        context.close(descriptor.fd).unwrap();
        assert_eq!(offset, expected.len());
        assert_eq!(actual, expected);
    }

    #[test]
    fn recursive_file_operations_preserve_source_until_copy_finishes() {
        let context = VfsContext::new();
        context.mkdir("/tree").unwrap();
        context.mkdir("/tree/nested").unwrap();
        context.replace_file("/tree/nested/file", b"data").unwrap();

        assert_eq!(
            context.copy_path("/tree", "/tree/child", true),
            Err(FsError::InvalidPath)
        );
        context.copy_path("/tree", "/copied", true).unwrap();
        assert!(context.exists("/tree/nested/file"));
        assert!(context.exists("/copied/nested/file"));

        context.move_path("/copied", "/moved", true).unwrap();
        assert!(!context.exists("/copied"));
        assert!(context.exists("/moved/nested/file"));
        context.remove_path("/moved", true).unwrap();
        assert!(!context.exists("/moved"));
    }
}
