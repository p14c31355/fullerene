# Genome — Public Trait API (v0.1)

> **Status: DRAFT — 凍結予定**
>
> VFS層の公開インターフェース。`FileSystem` trait が唯一の抽象であり、
> すべてのファイルシステム実装（MemFileSystem, FatFileSystem, exFAT 等）は
> このtraitを実装する。

---

## 1. FileSystem — ファイルシステム抽象

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

**v0.1 凍結範囲**: この10メソッドを超えて拡張しない。必要ならユーティリティは別traitに分離する。

---

## 2. 関連型

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

## 3. Vfs — マウントテーブルディスパッチャ

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
    pub fn create(&mut self, path: &str) -> Option<u64>;
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

**マウント戦略**: 最長一致のマウントポイント検索 (longest-prefix match)。

---

## 4. 実装 (v0.1 同梱)

| 実装 | 説明 |
|---|---|
| `MemFileSystem` | B-tree ベースのインメモリ tmpfs。Genome に同梱。 |
| `FatFileSystem` | FAT32 バックエンド。kernel の `drivers/fat.rs` に配置。 |

---

## 変更履歴

| 日付 | 変更 |
|---|---|
| 2026-07-13 | v0.1 初版 |
