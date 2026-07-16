use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;

use genome::block::BlockDevice;
use genome::fs::FsError;
use genome::vfs::{FileDescriptor, FileSystem, FileSystemCapabilities, InodeType, VNode};
use nitrogen::driver_api::DriverBox;

static DEVICE_REGISTRY: Mutex<BTreeMap<String, DriverBox>> = Mutex::new(BTreeMap::new());
const NULL_DEVICE: &str = "null";

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
    fn capabilities(&self) -> FileSystemCapabilities {
        FileSystemCapabilities::new(false, false, false, false, true)
    }

    fn open(&mut self, path: &str, _flags: u32) -> Option<FileDescriptor> {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            return None;
        }
        if path != NULL_DEVICE
            && !DEVICE_REGISTRY.lock().contains_key(path)
            && !block_device_exists(path)
        {
            return None;
        }
        let ino = stable_ino(path);
        let fd = next_fd();
        let name = path.to_string();
        FD_TABLE.lock().push(FdEntry {
            name,
            fd,
            offset: 0,
        });
        Some(FileDescriptor {
            fd,
            ino,
            offset: 0,
            flags: 0,
        })
    }

    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        let (name, entry_offset) = {
            let table = FD_TABLE.lock();
            let entry = table
                .iter()
                .find(|e| e.fd == fd)
                .ok_or(FsError::InvalidFileDescriptor)?;
            (entry.name.clone(), entry.offset)
        };
        if name == NULL_DEVICE {
            return Ok(0);
        }
        // TODO: registry lock held during I/O blocks other registry ops.
        // Refactor to use ref-counted driver handles so the lock is dropped before I/O.
        let (result, new_offset) = {
            let registry = DEVICE_REGISTRY.lock();
            match registry.get(&name) {
                Some(DriverBox::Storage(drv)) => {
                    let bs = drv.block_size() as u64;
                    if bs == 0 || buf.is_empty() {
                        (Ok(0), entry_offset)
                    } else {
                        let block_off = entry_offset % bs;
                        let lba = entry_offset / bs;
                        let count = block_off
                            .checked_add(buf.len() as u64)
                            .map(|sum| sum.div_ceil(bs).max(1))
                            .unwrap_or(1);
                        let actual = count.min(64) as usize;
                        let read_bytes = actual * bs as usize;
                        let mut tmp = alloc::vec![0u8; read_bytes];
                        match drv.read_blocks(lba, actual, &mut tmp) {
                            Ok(_) => {
                                let block_off =
                                    usize::try_from(block_off).map_err(|_| FsError::InvalidSeek)?;
                                let n = buf.len().min(read_bytes.saturating_sub(block_off));
                                buf[..n].copy_from_slice(&tmp[block_off..block_off + n]);
                                let new_offset = entry_offset
                                    .checked_add(n as u64)
                                    .ok_or(FsError::InvalidSeek)?;
                                (Ok(n), new_offset)
                            }
                            Err(_) => (Err(FsError::NotSupported), entry_offset),
                        }
                    }
                }
                Some(DriverBox::Network(drv)) => match drv.receive(buf) {
                    Ok(n) => {
                        let new_offset = entry_offset
                            .checked_add(n as u64)
                            .ok_or(FsError::InvalidSeek)?;
                        (Ok(n), new_offset)
                    }
                    Err(_) => (Err(FsError::NotSupported), entry_offset),
                },
                Some(DriverBox::Audio(_))
                | Some(DriverBox::UsbHost(_))
                | Some(DriverBox::Display(_))
                | Some(DriverBox::None)
                | None => (Err(FsError::NotSupported), entry_offset),
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
            let entry = table
                .iter()
                .find(|e| e.fd == fd)
                .ok_or(FsError::InvalidFileDescriptor)?;
            (entry.name.clone(), entry.offset)
        };
        if name == NULL_DEVICE {
            if let Some(entry) = FD_TABLE.lock().iter_mut().find(|entry| entry.fd == fd) {
                entry.offset = entry.offset.saturating_add(data.len() as u64);
            }
            return Ok(data.len());
        }
        let (result, new_offset) = {
            let registry = DEVICE_REGISTRY.lock();
            match registry.get(&name) {
                Some(DriverBox::Storage(drv)) => {
                    let bs = drv.block_size() as u64;
                    if bs == 0 || data.is_empty() {
                        (Ok(0), entry_offset)
                    } else {
                        let block_off = entry_offset % bs;
                        let lba = entry_offset / bs;
                        let count = block_off
                            .checked_add(data.len() as u64)
                            .map(|sum| sum.div_ceil(bs).max(1))
                            .unwrap_or(1);
                        let actual = count.min(64) as usize;
                        let write_bytes = actual * bs as usize;
                        let block_off =
                            usize::try_from(block_off).map_err(|_| FsError::InvalidSeek)?;
                        let n = data.len().min(write_bytes.saturating_sub(block_off));
                        let mut tmp = alloc::vec![0u8; write_bytes];
                        if block_off != 0 || n != write_bytes {
                            if drv.read_blocks(lba as u64, actual, &mut tmp).is_err() {
                                return Err(FsError::NotSupported);
                            }
                        }
                        tmp[block_off..block_off + n].copy_from_slice(&data[..n]);
                        match drv.write_blocks(lba, actual, &tmp) {
                            Ok(_) => {
                                let new_offset = entry_offset
                                    .checked_add(n as u64)
                                    .ok_or(FsError::InvalidSeek)?;
                                (Ok(n), new_offset)
                            }
                            Err(_) => (Err(FsError::NotSupported), entry_offset),
                        }
                    }
                }
                Some(DriverBox::Network(drv)) => match drv.send(data) {
                    Ok(_) => {
                        let new_offset = entry_offset
                            .checked_add(data.len() as u64)
                            .ok_or(FsError::InvalidSeek)?;
                        (Ok(data.len()), new_offset)
                    }
                    Err(_) => (Err(FsError::NotSupported), entry_offset),
                },
                Some(DriverBox::Audio(drv)) => match drv.play(data) {
                    Ok(_) => {
                        let new_offset = entry_offset
                            .checked_add(data.len() as u64)
                            .ok_or(FsError::InvalidSeek)?;
                        (Ok(data.len()), new_offset)
                    }
                    Err(_) => (Err(FsError::NotSupported), entry_offset),
                },
                Some(DriverBox::UsbHost(_))
                | Some(DriverBox::Display(_))
                | Some(DriverBox::None)
                | None => (Err(FsError::NotSupported), entry_offset),
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
        if table.len() == before {
            Err(FsError::InvalidFileDescriptor)
        } else {
            Ok(())
        }
    }

    fn seek(&mut self, fd: u32, pos: u64) -> Result<(), FsError> {
        let mut table = FD_TABLE.lock();
        let entry = table
            .iter_mut()
            .find(|e| e.fd == fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
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
        let mut names = BTreeSet::new();
        names.insert(String::from(NULL_DEVICE));
        names.extend(DEVICE_REGISTRY.lock().keys().cloned());
        names.extend(BLOCK_DEVICE_REGISTRY.lock().keys().cloned());
        Ok(names
            .into_iter()
            .map(|name| VNode {
                name,
                size: 0,
                is_dir: false,
            })
            .collect())
    }

    fn exists(&mut self, path: &str) -> bool {
        let path = path.trim_start_matches('/');
        path.is_empty()
            || path == NULL_DEVICE
            || DEVICE_REGISTRY.lock().contains_key(path)
            || block_device_exists(path)
    }
}

struct FdEntry {
    name: String,
    fd: u32,
    offset: u64,
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
// Maps persistent device names (e.g. "usb0", "sd0") to shared devices with
// an explicit exclusive lease bit. Filesystems receive a proxy, so dropping a
// mounted filesystem never destroys the registry-owned device.

struct BlockDeviceEntry {
    device: Arc<Mutex<Box<dyn BlockDevice>>>,
    leased: bool,
}

struct BlockDeviceLease {
    device: Arc<Mutex<Box<dyn BlockDevice>>>,
}

impl BlockDevice for BlockDeviceLease {
    fn read_sectors(
        &mut self,
        lba: u64,
        count: u16,
        buf: &mut [u8],
    ) -> Result<(), genome::block::BlockError> {
        self.device.lock().read_sectors(lba, count, buf)
    }

    fn write_sectors(
        &mut self,
        lba: u64,
        count: u16,
        buf: &[u8],
    ) -> Result<(), genome::block::BlockError> {
        self.device.lock().write_sectors(lba, count, buf)
    }

    fn sector_size(&self) -> u32 {
        self.device.lock().sector_size()
    }

    fn total_sectors(&self) -> u64 {
        self.device.lock().total_sectors()
    }
}

static BLOCK_DEVICE_REGISTRY: Mutex<BTreeMap<alloc::string::String, BlockDeviceEntry>> =
    Mutex::new(BTreeMap::new());

pub fn register_block_device(name: alloc::string::String, device: Box<dyn BlockDevice>) {
    BLOCK_DEVICE_REGISTRY.lock().insert(
        name,
        BlockDeviceEntry {
            device: Arc::new(Mutex::new(device)),
            leased: false,
        },
    );
}

pub fn unregister_block_device(name: &str) {
    BLOCK_DEVICE_REGISTRY.lock().remove(name);
}

/// Lease a block device to a filesystem while preserving its `/dev` identity.
pub fn lease_block_device(name: &str) -> Option<Box<dyn BlockDevice>> {
    let mut registry = BLOCK_DEVICE_REGISTRY.lock();
    let entry = registry.get_mut(name)?;
    if entry.leased {
        return None;
    }
    entry.leased = true;
    Some(Box::new(BlockDeviceLease {
        device: Arc::clone(&entry.device),
    }))
}

/// Return an exclusive filesystem lease while retaining the persistent device.
pub fn return_block_device_lease(name: &str) -> bool {
    let mut registry = BLOCK_DEVICE_REGISTRY.lock();
    let Some(entry) = registry.get_mut(name) else {
        return false;
    };
    let was_leased = entry.leased;
    entry.leased = false;
    was_leased
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
        .is_some_and(|entry| !entry.leased)
}

#[cfg(test)]
mod tests {
    use super::*;
    use genome::block::BlockError;

    struct MemoryBlockDevice {
        sector: [u8; 16],
    }

    impl BlockDevice for MemoryBlockDevice {
        fn read_sectors(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
            if lba != 0 || count != 1 || buf.len() < self.sector.len() {
                return Err(BlockError::LbaOverflow);
            }
            buf[..self.sector.len()].copy_from_slice(&self.sector);
            Ok(())
        }

        fn write_sectors(&mut self, lba: u64, count: u16, buf: &[u8]) -> Result<(), BlockError> {
            if lba != 0 || count != 1 || buf.len() < self.sector.len() {
                return Err(BlockError::LbaOverflow);
            }
            self.sector.copy_from_slice(&buf[..16]);
            Ok(())
        }

        fn sector_size(&self) -> u32 {
            self.sector.len() as u32
        }

        fn total_sectors(&self) -> u64 {
            1
        }
    }

    #[test]
    fn returned_lease_can_be_reacquired_without_losing_device_state() {
        const NAME: &str = "test-returned-lease";
        unregister_block_device(NAME);
        register_block_device(
            String::from(NAME),
            Box::new(MemoryBlockDevice { sector: [0; 16] }),
        );

        let mut first = lease_block_device(NAME).unwrap();
        assert!(!block_device_available(NAME));
        assert!(lease_block_device(NAME).is_none());
        first.write_sectors(0, 1, &[9; 16]).unwrap();
        drop(first);
        assert!(return_block_device_lease(NAME));

        let mut second = lease_block_device(NAME).unwrap();
        let mut data = [0; 16];
        second.read_sectors(0, 1, &mut data).unwrap();
        assert_eq!(data, [9; 16]);
        drop(second);
        return_block_device_lease(NAME);
        unregister_block_device(NAME);
    }
}
