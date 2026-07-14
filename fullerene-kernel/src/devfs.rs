use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;

use genome::block::BlockDevice;
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
            drop(registry);
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
        let (name, entry_offset) = {
            let table = FD_TABLE.lock();
            let entry = table.iter().find(|e| e.fd == fd).ok_or(FsError::InvalidFileDescriptor)?;
            (entry.name.clone(), entry.offset)
        };
        // TODO: registry lock held during I/O blocks other registry ops.
        // Refactor to use ref-counted driver handles so the lock is dropped before I/O.
        let (result, new_offset) = {
            let registry = DEVICE_REGISTRY.lock();
            match registry.get(&name) {
                Some(DriverBox::Storage(drv)) => {
                    let bs = drv.block_size() as usize;
                    if bs == 0 || buf.is_empty() { (Ok(0), entry_offset) }
                    else {
                        let block_off = entry_offset % bs;
                        let lba = entry_offset / bs;
                        let count = block_off.checked_add(buf.len()).map(|sum| sum.div_ceil(bs).max(1)).unwrap_or(1);
                        let actual = count.min(64);
                        let read_bytes = actual * bs;
                        let mut tmp = alloc::vec![0u8; read_bytes];
                        match drv.read_blocks(lba as u64, actual, &mut tmp) {
                            Ok(_) => {
                                let n = buf.len().min(read_bytes.saturating_sub(block_off));
                                buf[..n].copy_from_slice(&tmp[block_off..block_off + n]);
                                (Ok(n), entry_offset + n)
                            }
                            Err(_) => (Err(FsError::NotSupported), entry_offset),
                        }
                    }
                }
                Some(DriverBox::Network(drv)) => {
                    match drv.receive(buf) {
                        Ok(n) => (Ok(n), entry_offset + n),
                        Err(_) => (Err(FsError::NotSupported), entry_offset),
                    }
                }
                Some(DriverBox::Audio(_)) | Some(DriverBox::UsbHost(_)) |
                Some(DriverBox::Display(_)) | Some(DriverBox::None) | None =>
                    (Err(FsError::NotSupported), entry_offset),
            }
        };
        if result.is_ok() {
            let mut table = FD_TABLE.lock();
            if let Some(e) = table.iter_mut().find(|e| e.fd == fd) {
                e.offset = new_offset;
            }
        }
        result
    }

    fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, FsError> {
        let (name, entry_offset) = {
            let table = FD_TABLE.lock();
            let entry = table.iter().find(|e| e.fd == fd).ok_or(FsError::InvalidFileDescriptor)?;
            (entry.name.clone(), entry.offset)
        };
        let (result, new_offset) = {
            let registry = DEVICE_REGISTRY.lock();
            match registry.get(&name) {
                Some(DriverBox::Storage(drv)) => {
                    let bs = drv.block_size() as usize;
                    if bs == 0 || data.is_empty() { (Ok(0), entry_offset) }
                    else {
                        let block_off = entry_offset % bs;
                        let lba = entry_offset / bs;
                        let count = block_off.checked_add(data.len()).map(|sum| sum.div_ceil(bs).max(1)).unwrap_or(1);
                        let actual = count.min(64);
                        let write_bytes = actual * bs;
                        let n = data.len().min(write_bytes.saturating_sub(block_off));
                        let mut tmp = alloc::vec![0u8; write_bytes];
                        if block_off != 0 || n != write_bytes {
                            if drv.read_blocks(lba as u64, actual, &mut tmp).is_err() {
                                return Err(FsError::NotSupported);
                            }
                        }
                        tmp[block_off..block_off + n].copy_from_slice(&data[..n]);
                        match drv.write_blocks(lba as u64, actual, &tmp) {
                            Ok(_) => (Ok(n), entry_offset + n),
                            Err(_) => (Err(FsError::NotSupported), entry_offset),
                        }
                    }
                }
                Some(DriverBox::Network(drv)) => {
                    match drv.send(data) {
                        Ok(_) => (Ok(data.len()), entry_offset + data.len()),
                        Err(_) => (Err(FsError::NotSupported), entry_offset),
                    }
                }
                Some(DriverBox::Audio(drv)) => {
                    match drv.play(data) {
                        Ok(_) => (Ok(data.len()), entry_offset + data.len()),
                        Err(_) => (Err(FsError::NotSupported), entry_offset),
                    }
                }
                Some(DriverBox::UsbHost(_)) | Some(DriverBox::Display(_)) |
                Some(DriverBox::None) | None => (Err(FsError::NotSupported), entry_offset),
            }
        };
        if result.is_ok() {
            let mut table = FD_TABLE.lock();
            if let Some(e) = table.iter_mut().find(|e| e.fd == fd) {
                e.offset = new_offset;
            }
        }
        result
    }

    fn close(&mut self, fd: u32) -> Result<(), FsError> {
        let mut table = FD_TABLE.lock();
        let before = table.len();
        table.retain(|e| e.fd != fd);
        if table.len() == before { Err(FsError::InvalidFileDescriptor) } else { Ok(()) }
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

// ── Block device registry ───────────────────────────────────
//
// Maps persistent device names (e.g. "usb0", "sd0") to an optional device
// lease. `None` means a mounted filesystem currently owns the device, while
// the `/dev` identity remains enumerable.

pub static BLOCK_DEVICE_REGISTRY: Mutex<
    BTreeMap<alloc::string::String, Option<Box<dyn BlockDevice>>>,
> = Mutex::new(BTreeMap::new());

pub fn register_block_device(name: alloc::string::String, device: Box<dyn BlockDevice>) {
    BLOCK_DEVICE_REGISTRY.lock().insert(name, Some(device));
}

pub fn unregister_block_device(name: &str) {
    BLOCK_DEVICE_REGISTRY.lock().remove(name);
}

/// Lease a block device to a filesystem while preserving its `/dev` identity.
pub fn lease_block_device(name: &str) -> Option<Box<dyn BlockDevice>> {
    BLOCK_DEVICE_REGISTRY.lock().get_mut(name)?.take()
}

pub fn list_block_device_names() -> alloc::vec::Vec<alloc::string::String> {
    BLOCK_DEVICE_REGISTRY.lock().keys().cloned().collect()
}

pub fn block_device_exists(name: &str) -> bool {
    BLOCK_DEVICE_REGISTRY.lock().contains_key(name)
}

pub fn block_device_available(name: &str) -> bool {
    BLOCK_DEVICE_REGISTRY
        .lock()
        .get(name)
        .is_some_and(|device| device.is_some())
}
