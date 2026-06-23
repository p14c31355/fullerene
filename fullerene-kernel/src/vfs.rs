//! VFS — re-exports from the `genome` crate plus kernel-level free functions.
//!
//! The core VFS types ([`FileSystem`], [`MemFileSystem`], [`Vfs`] dispatcher)
//! live in the `genome` crate.  This module re-exports them and adds the
//! free‑standing functions (`open`, `read`, `write`, …) that delegate to
//! the singleton [`VfsContext`] inside [`KernelContext`](crate::contexts::kernel::KernelContext).

use alloc::vec::Vec;

pub use genome::vfs::{
    FileDescriptor, FileSystem, InodeType, MemFileSystem, VNode, Vfs,
};

// ── Public API — delegated to VfsContext ────────────────────────

pub fn init() {
    crate::contexts::vfs::init_vfs();
}

pub fn mount(device: &str, mount_point: &str, fs_type: &str) -> Result<(), &'static str> {
    crate::contexts::vfs::mount(device, mount_point, fs_type)
}

pub fn unmount(mount_point: &str) -> Result<bool, &'static str> {
    crate::contexts::vfs::unmount(mount_point)
}

pub fn open(path: &str, flags: u32) -> Result<FileDescriptor, &'static str> {
    crate::contexts::vfs::open(path, flags)
}

pub fn read(fd: u32, buf: &mut [u8]) -> Result<usize, &'static str> {
    crate::contexts::vfs::read(fd, buf)
}

pub fn write(fd: u32, data: &[u8]) -> Result<usize, &'static str> {
    crate::contexts::vfs::write(fd, data)
}

pub fn close(fd: u32) -> Result<(), &'static str> {
    crate::contexts::vfs::close(fd)
}

pub fn readdir(path: &str) -> Result<Vec<VNode>, &'static str> {
    crate::contexts::vfs::readdir(path)
}

pub fn seek(fd: u32, pos: usize) -> Result<(), &'static str> {
    crate::contexts::vfs::seek(fd, pos)
}

pub fn create(path: &str) -> Result<FileDescriptor, &'static str> {
    crate::contexts::vfs::create(path)
}

pub fn mkdir(path: &str) -> Result<(), &'static str> {
    crate::contexts::vfs::mkdir(path)
}

pub fn unlink(path: &str) -> Result<(), &'static str> {
    crate::contexts::vfs::unlink(path)
}

pub fn exists(path: &str) -> bool {
    crate::contexts::vfs::exists(path)
}

pub fn working_directory() -> Result<alloc::string::String, &'static str> {
    crate::contexts::vfs::working_directory()
}

pub fn change_directory(path: &str) -> Result<(), &'static str> {
    crate::contexts::vfs::change_directory(path)
}
