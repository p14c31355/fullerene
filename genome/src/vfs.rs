use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

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
    size: usize,
}

impl Inode {
    fn new(ino: u64, name: &str, kind: InodeType, parent: u64) -> Self {
        let _ = ino;
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
    pub offset: usize,
    pub flags: u32,
}

#[derive(Debug, Clone)]
pub struct VNode {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

pub trait FileSystem: Send {
    fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor>;
    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, &'static str>;
    fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, &'static str>;
    fn close(&mut self, fd: u32) -> Result<(), &'static str>;
    fn seek(&mut self, fd: u32, pos: usize) -> Result<(), &'static str>;
    fn create(&mut self, path: &str, kind: InodeType) -> Option<u64>;
    fn mkdir(&mut self, path: &str) -> Result<(), &'static str>;
    fn unlink(&mut self, path: &str) -> Result<(), &'static str>;
    fn readdir(&mut self, path: &str) -> Result<Vec<VNode>, &'static str>;
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

    fn lookup(&self, path: &str) -> Option<u64> {
        self.lookup_from(path, 1, 0)
    }

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
            if *comp == "." {
                // current stays the same
            } else if *comp == ".." {
                let ino = self.inodes.get(&current)?;
                current = if ino.parent == 0 { 1 } else { ino.parent };
            } else {
                let ino = self.inodes.get(&current)?;
                let child = ino.children.iter().find(|&&c| {
                    self.inodes
                        .get(&c)
                        .map_or(false, |i| i.name.as_str() == *comp)
                })?;
                current = *child;
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
            .find(|&&c| {
                self.inodes
                    .get(&c)
                    .map_or(false, |i| i.name.as_str() == name)
            })
            .copied()
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
        let new_len = off.checked_add(data.len()).ok_or("integer overflow")?;
        if new_len > ino.data.len() {
            ino.data.resize(new_len, 0);
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

    fn readdir(&mut self, path: &str) -> Result<Vec<VNode>, &'static str> {
        let ino = self.lookup(path).ok_or("not found")?;
        let inode = self.inodes.get(&ino).ok_or("not found")?;
        if inode.kind != InodeType::Directory {
            return Err("not a directory");
        }
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

    fn exists(&mut self, path: &str) -> bool {
        self.lookup(path).is_some()
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

    pub fn working_directory(&self) -> &str {
        &self.wd
    }

    pub fn change_directory(&mut self, path: &str) -> Result<(), &'static str> {
        let resolved = self.resolve_path(path);
        let (fs, remaining) = self.find_fs(&resolved).ok_or("not found")?;
        let _entries = fs.readdir(&remaining).map_err(|_| "not a directory")?;
        self.wd = resolved;
        Ok(())
    }

    pub fn mount(
        &mut self,
        mount_point: &str,
        fs: Box<dyn FileSystem>,
    ) -> Result<(), &'static str> {
        let mp = normalize_path(mount_point);
        if mp != "/" {
            let (target_fs, remaining) = self.find_fs(&mp).ok_or("mount point not found")?;
            if !target_fs.exists(&remaining) {
                return Err("mount point not found");
            }
        }
        self.mounts.retain(|m| m.mount_point != mp);
        self.mounts.push(MountEntry {
            mount_point: mp,
            fs,
        });
        self.mounts
            .sort_by(|a, b| b.mount_point.len().cmp(&a.mount_point.len()));
        Ok(())
    }

    pub fn unmount(&mut self, mount_point: &str) -> Result<bool, &'static str> {
        let mp = normalize_path(mount_point);
        if mp == "/" {
            return Err("cannot unmount root");
        }
        let len_before = self.mounts.len();
        self.mounts.retain(|m| m.mount_point != mp);
        Ok(self.mounts.len() < len_before)
    }

    pub fn find_fs_index(&self, path: &str) -> Option<usize> {
        let absolute_path = self.resolve_path(path);
        let path = if absolute_path.starts_with('/') {
            &absolute_path
        } else {
            return None;
        };
        for (idx, entry) in self.mounts.iter().enumerate() {
            let mp = &entry.mount_point;
            if mp == "/" {
                return Some(idx);
            }
            let mp_with_slash = alloc::format!("{}/", mp.as_str());
            if path == mp.as_str() || path.starts_with(&mp_with_slash) {
                return Some(idx);
            }
        }
        None
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

        for entry in &mut self.mounts {
            let mp = &entry.mount_point;
            if mp == "/" {
                let remaining = path[1..].to_string();
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

    pub fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor> {
        let resolved = self.resolve_path(path);
        let (fs, remaining) = self.find_fs(&resolved)?;
        fs.open(&remaining, flags)
    }

    pub fn read_at(
        &mut self,
        mount_idx: usize,
        fd: u32,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        self.mounts
            .get_mut(mount_idx)
            .ok_or("bad mount index")?
            .fs
            .read(fd, buf)
    }

    pub fn write_at(
        &mut self,
        mount_idx: usize,
        fd: u32,
        data: &[u8],
    ) -> Result<usize, &'static str> {
        self.mounts
            .get_mut(mount_idx)
            .ok_or("bad mount index")?
            .fs
            .write(fd, data)
    }

    pub fn close_at(&mut self, mount_idx: usize, fd: u32) -> Result<(), &'static str> {
        self.mounts
            .get_mut(mount_idx)
            .ok_or("bad mount index")?
            .fs
            .close(fd)
    }

    pub fn seek_at(&mut self, mount_idx: usize, fd: u32, pos: usize) -> Result<(), &'static str> {
        self.mounts
            .get_mut(mount_idx)
            .ok_or("bad mount index")?
            .fs
            .seek(fd, pos)
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
