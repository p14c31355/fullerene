use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;

use genome::fs::FsError;
use genome::vfs::{FileDescriptor, FileSystem, InodeType, VNode};
use nitrogen::driver_api::DriverBox;

static DEVICE_REGISTRY: Mutex<BTreeMap<String, DriverBox>> = Mutex::new(BTreeMap::new());

pub fn register_driver(name: &str, driver: DriverBox) {
    DEVICE_REGISTRY.lock().insert(name.to_string(), driver);
}

pub fn unregister_driver(name: &str) {
    DEVICE_REGISTRY.lock().remove(name);
}

pub fn driver_exists(name: &str) -> bool {
    DEVICE_REGISTRY.lock().contains_key(name)
}

pub fn list_devices() -> Vec<String> {
    DEVICE_REGISTRY.lock().keys().cloned().collect()
}

pub struct DevFs;

impl DevFs {
    pub const fn new() -> Self {
        Self
    }
}

impl FileSystem for DevFs {
    fn open(&mut self, path: &str, _flags: u32) -> Option<FileDescriptor> {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            return None;
        }
        let registry = DEVICE_REGISTRY.lock();
        if registry.contains_key(path) {
            let ino = stable_ino(path);
            let fd = next_fd();
            let name = path.to_string();
            FD_TABLE.lock().push(FdEntry { name, fd, offset: 0 });
            Some(FileDescriptor { fd, ino, offset: 0, flags: 0 })
        } else {
            None
        }
    }

    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        let entry_idx = FD_TABLE.lock().iter().position(|e| e.fd == fd).ok_or(FsError::InvalidFileDescriptor)?;
        let name = FD_TABLE.lock()[entry_idx].name.clone();
        let registry = DEVICE_REGISTRY.lock();
        let driver = registry.get(&name).ok_or(FsError::FileNotFound)?;
        let mut entry_offset = FD_TABLE.lock()[entry_idx].offset;
        let result = match driver {
            DriverBox::Storage(drv) => {
                let bs = drv.block_size() as usize;
                let lba = entry_offset / bs;
                let count = buf.len().div_ceil(bs).max(1);
                let actual = count.min(64);
                let read_bytes = actual * bs;
                let mut tmp = alloc::vec![0u8; read_bytes];
                drv.read_blocks(lba as u64, actual, &mut tmp).map_err(|_| FsError::NotSupported)?;
                let n = buf.len().min(read_bytes);
                buf[..n].copy_from_slice(&tmp[..n]);
                entry_offset += n;
                Ok(n)
            }
            DriverBox::Network(drv) => {
                let n = drv.receive(buf).map_err(|_| FsError::NotSupported)?;
                entry_offset += n;
                Ok(n)
            }
            DriverBox::Audio(_) => Err(FsError::NotSupported),
            DriverBox::UsbHost(_) => Err(FsError::NotSupported),
            DriverBox::Display(_) => Err(FsError::NotSupported),
            DriverBox::None => Err(FsError::FileNotFound),
        };
        if result.is_ok() {
            FD_TABLE.lock()[entry_idx].offset = entry_offset;
        }
        result
    }

    fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, FsError> {
        let entry_idx = FD_TABLE.lock().iter().position(|e| e.fd == fd).ok_or(FsError::InvalidFileDescriptor)?;
        let name = FD_TABLE.lock()[entry_idx].name.clone();
        let registry = DEVICE_REGISTRY.lock();
        let driver = registry.get(&name).ok_or(FsError::FileNotFound)?;
        let mut entry_offset = FD_TABLE.lock()[entry_idx].offset;
        let result = match driver {
            DriverBox::Storage(drv) => {
                let bs = drv.block_size() as usize;
                let lba = entry_offset / bs;
                let count = data.len().div_ceil(bs).max(1);
                let actual = count.min(64);
                let write_bytes = actual * bs;
                let mut tmp = alloc::vec![0u8; write_bytes];
                tmp[..data.len().min(write_bytes)].copy_from_slice(&data[..data.len().min(write_bytes)]);
                drv.write_blocks(lba as u64, actual, &tmp).map_err(|_| FsError::NotSupported)?;
                entry_offset += data.len();
                Ok(data.len())
            }
            DriverBox::Network(drv) => {
                drv.send(data).map_err(|_| FsError::NotSupported)?;
                entry_offset += data.len();
                Ok(data.len())
            }
            DriverBox::Audio(drv) => {
                drv.play(data).map_err(|_| FsError::NotSupported)?;
                entry_offset += data.len();
                Ok(data.len())
            }
            DriverBox::UsbHost(_) => Err(FsError::NotSupported),
            DriverBox::Display(_) => Err(FsError::NotSupported),
            DriverBox::None => Err(FsError::FileNotFound),
        };
        if result.is_ok() {
            FD_TABLE.lock()[entry_idx].offset = entry_offset;
        }
        result
    }

    fn close(&mut self, fd: u32) -> Result<(), FsError> {
        FD_TABLE.lock().retain(|e| e.fd != fd);
        Ok(())
    }

    fn seek(&mut self, fd: u32, pos: usize) -> Result<(), FsError> {
        let mut table = FD_TABLE.lock();
        let entry = table.iter_mut().find(|e| e.fd == fd).ok_or(FsError::InvalidFileDescriptor)?;
        entry.offset = pos;
        Ok(())
    }

    fn create(&mut self, _path: &str, _kind: InodeType) -> Option<u64> {
        None
    }

    fn mkdir(&mut self, _path: &str) -> Result<(), FsError> {
        Err(FsError::NotSupported)
    }

    fn unlink(&mut self, _path: &str) -> Result<(), FsError> {
        Err(FsError::NotSupported)
    }

    fn readdir(&mut self, path: &str) -> Result<Vec<VNode>, FsError> {
        let path = path.trim_start_matches('/');
        if !path.is_empty() {
            return Err(FsError::NotADirectory);
        }
        let registry = DEVICE_REGISTRY.lock();
        Ok(registry.keys().map(|name| VNode {
            name: name.clone(),
            size: 0,
            is_dir: false,
        }).collect())
    }

    fn exists(&mut self, path: &str) -> bool {
        let path = path.trim_start_matches('/');
        path.is_empty() || DEVICE_REGISTRY.lock().contains_key(path)
    }
}

struct FdEntry {
    name: String,
    fd: u32,
    offset: usize,
}

static FD_TABLE: Mutex<Vec<FdEntry>> = Mutex::new(Vec::new());
static NEXT_FD: AtomicU32 = AtomicU32::new(100);

fn next_fd() -> u32 {
    NEXT_FD.fetch_add(1, Ordering::Relaxed)
}

fn stable_ino(name: &str) -> u64 {
    let mut h: u64 = 0;
    for b in name.bytes() {
        h = h.wrapping_mul(31).wrapping_add(b as u64);
    }
    h | 0x1000_0000_0000_0000
}
