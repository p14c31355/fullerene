# Genome — Public Trait API (v0.1)

> **Status: DRAFT — Subject to Freeze**
>
> Public interface of the VFS layer. The `FileSystem` trait is the sole abstraction,
> and all filesystem implementations (MemFileSystem, FatFileSystem, exFAT, etc.)
> implement this trait.

---

## 1. FileSystem — Filesystem Abstraction

`genome::vfs::FileSystem`

```rust
pub trait FileSystem: Send {
    fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor>;
    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, FsError>;
    fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, FsError>;
    fn close(&mut self, fd: u32) -> Result<(), FsError>;
    fn seek(&mut self, fd: u32, pos: usize) -> Result<(), FsError>;
    fn create(&mut self, path: &str, kind: InodeType) -> Option<u64>;
    fn mkdir(&mut self, path: &str) -> Result<(), FsError>;
    fn unlink(&mut self, path: &str) -> Result<(), FsError>;
    fn readdir(&mut self, path: &str) -> Result<Vec<VNode>, FsError>;
    fn exists(&mut self, path: &str) -> bool;
}
```

**v0.1 freeze scope**: Do not extend beyond these 10 methods. If necessary, utilities should be split into separate traits.

---

## 2. Associated Types

### FsError

`genome::fs::FsError`

```rust
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
    NotSupported,
    InvalidInput,
}
```

### InodeType

`genome::vfs::InodeType`

```rust
pub enum InodeType {
    File,
    Directory,
    Symlink,
}
```

### VNode

`genome::vfs::VNode`

```rust
pub struct VNode {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}
```

### FileDescriptor

`genome::vfs::FileDescriptor`

```rust
pub struct FileDescriptor {
    pub fd: u32,
    pub ino: u64,
    pub offset: usize,
    pub flags: u32,
}
```

---

## 3. Vfs — Mount Table Dispatcher

`genome::vfs::Vfs`

```rust
pub struct Vfs { /* ... */ }

impl Vfs {
    pub fn new(root_fs: Box<dyn FileSystem>) -> Self;
    pub fn mount(&mut self, mount_point: &str, fs: Box<dyn FileSystem>) -> Result<(), FsError>;
    pub fn unmount(&mut self, mount_point: &str) -> Result<bool, FsError>;
    pub fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor>;
    pub fn read_at(&mut self, mount_idx: usize, fd: u32, buf: &mut [u8]) -> Result<usize, FsError>;
    pub fn write_at(&mut self, mount_idx: usize, fd: u32, data: &[u8]) -> Result<usize, FsError>;
    pub fn close_at(&mut self, mount_idx: usize, fd: u32) -> Result<(), FsError>;
    pub fn seek_at(&mut self, mount_idx: usize, fd: u32, pos: usize) -> Result<(), FsError>;
    pub fn create(&mut self, path: &str) -> Option<u64>; // Creates files only (InodeType::File)
    pub fn mkdir(&mut self, path: &str) -> Result<(), FsError>;
    pub fn unlink(&mut self, path: &str) -> Result<(), FsError>;
    pub fn readdir(&mut self, path: &str) -> Result<Vec<VNode>, FsError>;
    pub fn exists(&mut self, path: &str) -> bool;
    pub fn working_directory(&self) -> &str;
    pub fn change_directory(&mut self, path: &str) -> Result<(), FsError>;
    pub fn resolve_path(&self, path: &str) -> String;
    pub fn find_fs(&mut self, absolute_path: &str) -> Option<(&mut Box<dyn FileSystem>, String)>;
}
```

**Mount strategy**: Longest-prefix match for mount point resolution.

---

## 4. Implementations (bundled in v0.1)

| Implementation | Description |
|---|---|
| `MemFileSystem` | B-tree based in-memory tmpfs. Bundled with Genome. |
| `FatFileSystem` | FAT32 backend. Located in kernel `drivers/fat.rs`. |

---

## Changelog

| Date | Change |
|---|---|
| 2026-07-13 | v0.1 initial |
