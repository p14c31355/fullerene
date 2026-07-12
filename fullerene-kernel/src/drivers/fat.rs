//! FAT32/exFAT filesystem driver — thin wrapper around the `fatfs` crate.
//!
//! Implements the `FileSystem` trait over a block device via `fatfs`.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::str;

use fatfs::{self, DefaultTimeProvider, IoBase, IoError as FatIoError, LossyOemCpConverter, Read, Write, Seek, SeekFrom};

type FatType = fatfs::FileSystem<FatDevice>;
type FatDir<'a> = fatfs::Dir<'a, FatDevice, DefaultTimeProvider, LossyOemCpConverter>;
type FatFile<'a> = fatfs::File<'a, FatDevice, DefaultTimeProvider, LossyOemCpConverter>;
type FatError = fatfs::Error<FatBlockError>;

use crate::klog_fmt;
use crate::contexts::vfs::{FileDescriptor, FileSystem, InodeType, VNode};
use genome::fs::FsError;

#[repr(C, packed)]
struct MbrPartitionEntry {
    status: u8,
    chs_first: [u8; 3],
    partition_type: u8,
    chs_last: [u8; 3],
    lba_start: u32,
    sector_count: u32,
}

const MBR_SIGNATURE: u16 = 0xAA55;
const PARTITION_FAT32: u8 = 0x0B;
const PARTITION_FAT32_LBA: u8 = 0x0C;
const PARTITION_FAT16: u8 = 0x06;
const PARTITION_FAT16_LBA: u8 = 0x0E;
const PARTITION_EXFAT: u8 = 0x07;

pub fn find_fat_partition(device: &mut dyn BlockDevice) -> Result<u32, FsError> {
    let mut boot = [0u8; 512];
    device.read_sectors(0, 1, &mut boot)?;

    if is_exfat(&boot) {
        klog_fmt!("FAT: raw exFAT at LBA 0\n");
        return Ok(0);
    }
    let bps = u16::from_le_bytes([boot[11], boot[12]]);
    if bps == 512 || bps == 1024 || bps == 2048 || bps == 4096 {
        klog_fmt!("FAT: raw FAT32 at LBA 0 (bps={})\n", bps);
        return Ok(0);
    }

    let sig = u16::from_le_bytes([boot[0x1FE], boot[0x1FF]]);
    if sig != MBR_SIGNATURE {
        klog_fmt!("FAT: no MBR signature at LBA 0 (0x{:04X})\n", sig);
        return Ok(0);
    }

    let mut best_lba: Option<u32> = None;
    let mut best_sectors: u32 = 0;
    for i in 0..4 {
        let off = 0x1BE + i * 16;
        let entry_ptr = boot[off..].as_ptr() as *const MbrPartitionEntry;
        let ptype = unsafe { core::ptr::read_unaligned(&raw const (*entry_ptr).partition_type) };
        let lba_start = unsafe { core::ptr::read_unaligned(&raw const (*entry_ptr).lba_start) };
        let sector_count = unsafe { core::ptr::read_unaligned(&raw const (*entry_ptr).sector_count) };
        let is_fat = matches!(
            ptype,
            PARTITION_FAT32 | PARTITION_FAT32_LBA | PARTITION_FAT16 | PARTITION_FAT16_LBA | PARTITION_EXFAT
        );
        if is_fat && sector_count > best_sectors {
            best_lba = Some(lba_start);
            best_sectors = sector_count;
        }
    }
    if let Some(lba) = best_lba {
        klog_fmt!("FAT: selected partition at LBA {} ({} sectors)\n", lba, best_sectors);
        return Ok(lba);
    }
    klog_fmt!("FAT: no FAT partition found in MBR\n");
    Err(FsError::FileNotFound)
}

use spin::Mutex;

static BLOCK_DEVICES: Mutex<Vec<(&'static str, Box<dyn BlockDevice>)>> = Mutex::new(Vec::new());

pub fn register_block_device(name: &'static str, device: Box<dyn BlockDevice>) {
    BLOCK_DEVICES.lock().push((name, device));
    klog_fmt!("FAT: registered block device {}\n", name);
}

pub fn open_block_device(name: &str) -> Result<FatFileSystem, FsError> {
    let mut devices = BLOCK_DEVICES.lock();
    let pos = devices.iter().position(|(n, _)| *n == name).ok_or(FsError::FileNotFound)?;
    let (_, device) = devices.remove(pos);
    FatFileSystem::from_device(device)
}

pub trait BlockDevice: Send {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str>;
    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str>;
    fn sector_size(&self) -> u32;
    fn total_sectors(&self) -> u64;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockError {
    Device(&'static str),
    BufferTooSmall { required: usize, provided: usize },
    LbaOverflow,
    SectorNotFound,
}

impl From<&'static str> for BlockError {
    fn from(e: &'static str) -> Self { BlockError::Device(e) }
}

pub struct BlockCache<D: BlockDevice> {
    inner: D,
    bps: usize,
    entries: Vec<(Option<u32>, Vec<u8>)>,
    capacity: usize,
    next_victim: usize,
}

impl<D: BlockDevice> BlockCache<D> {
    pub fn new(inner: D, capacity: usize) -> Self {
        let bps = inner.sector_size() as usize;
        let mut entries = Vec::with_capacity(capacity);
        for _ in 0..capacity { entries.push((None, vec![0u8; bps])); }
        Self { inner, bps, entries, capacity, next_victim: 0 }
    }

    fn lookup(&self, lba: u32) -> Option<usize> {
        self.entries.iter().position(|(l, _)| *l == Some(lba))
    }

    pub fn read_sector(&mut self, lba: u32, buf: &mut [u8]) -> Result<(), BlockError> {
        if buf.len() < self.bps { return Err(BlockError::BufferTooSmall { required: self.bps, provided: buf.len() }); }
        if (lba as u64) >= self.inner.total_sectors() { return Err(BlockError::LbaOverflow); }
        if let Some(idx) = self.lookup(lba) { buf[..self.bps].copy_from_slice(&self.entries[idx].1); return Ok(()); }
        let idx = self.evict_slot();
        let entry = &mut self.entries[idx];
        self.inner.read_sectors(lba, 1, &mut entry.1)?;
        entry.0 = Some(lba);
        buf[..self.bps].copy_from_slice(&entry.1);
        Ok(())
    }

    pub fn get_sector(&mut self, lba: u32) -> Result<&[u8], BlockError> {
        if (lba as u64) >= self.inner.total_sectors() { return Err(BlockError::LbaOverflow); }
        if let Some(idx) = self.lookup(lba) { return Ok(&self.entries[idx].1); }
        let idx = self.evict_slot();
        let entry = &mut self.entries[idx];
        self.inner.read_sectors(lba, 1, &mut entry.1)?;
        entry.0 = Some(lba);
        Ok(&self.entries[idx].1)
    }

    fn evict_slot(&mut self) -> usize {
        if let Some(idx) = self.entries.iter().position(|(l, _)| l.is_none()) { return idx; }
        let idx = self.next_victim;
        self.next_victim = (self.next_victim + 1) % self.capacity;
        idx
    }

    pub fn write_sector(&mut self, lba: u32, buf: &[u8]) -> Result<(), BlockError> {
        if (lba as u64) >= self.inner.total_sectors() { return Err(BlockError::LbaOverflow); }
        if buf.len() < self.bps { return Err(BlockError::BufferTooSmall { required: self.bps, provided: buf.len() }); }
        if let Some(idx) = self.lookup(lba) { self.entries[idx].0 = None; }
        self.inner.write_sectors(lba, 1, buf).map_err(BlockError::Device)
    }

    pub fn sector_size(&self) -> u32 { self.bps as u32 }
    pub fn total_sectors(&self) -> u64 { self.inner.total_sectors() }
}

impl<D: BlockDevice> BlockDevice for BlockCache<D> {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str> {
        let count = count as usize;
        let needed = count.checked_mul(self.bps).ok_or("count * bps overflow")?;
        if buf.len() < needed { return Err("buffer too small for multi-sector read"); }
        let end_lba = (lba as u64) + (count as u64);
        if end_lba > self.inner.total_sectors() || end_lba > u32::MAX as u64 { return Err("LBA range exceeds device capacity or 32-bit limit"); }
        for i in 0..count {
            let off = i * self.bps;
            self.read_sector(lba + i as u32, &mut buf[off..off + self.bps]).map_err(|e| match e { BlockError::Device(s) => s, _ => "block cache error" })?;
        }
        Ok(())
    }
    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
        let count = count as usize;
        let needed = count.checked_mul(self.bps).ok_or("count * bps overflow")?;
        if buf.len() < needed { return Err("buffer too small for multi-sector write"); }
        let end_lba = (lba as u64) + (count as u64);
        if end_lba > self.inner.total_sectors() || end_lba > u32::MAX as u64 { return Err("LBA range exceeds device capacity or 32-bit limit"); }
        for i in 0..count {
            let off = i * self.bps;
            self.write_sector(lba + i as u32, &buf[off..off + self.bps]).map_err(|e| match e { BlockError::Device(s) => s, _ => "block cache error" })?;
        }
        Ok(())
    }
    fn sector_size(&self) -> u32 { self.bps as u32 }
    fn total_sectors(&self) -> u64 { self.inner.total_sectors() }
}

pub struct PartitionBlockDevice {
    inner: Box<dyn BlockDevice>,
    offset: u32,
}

impl BlockDevice for PartitionBlockDevice {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str> { self.inner.read_sectors(lba + self.offset, count, buf) }
    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> { self.inner.write_sectors(lba + self.offset, count, buf) }
    fn sector_size(&self) -> u32 { self.inner.sector_size() }
    fn total_sectors(&self) -> u64 { self.inner.total_sectors().saturating_sub(self.offset as u64) }
}

impl BlockDevice for Box<dyn BlockDevice> {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str> { (**self).read_sectors(lba, count, buf) }
    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> { (**self).write_sectors(lba, count, buf) }
    fn sector_size(&self) -> u32 { (**self).sector_size() }
    fn total_sectors(&self) -> u64 { (**self).total_sectors() }
}

// ── fatfs device adapter ─────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FatBlockError {
    Device(&'static str),
    UnexpectedEof,
    WriteZero,
}

impl core::fmt::Display for FatBlockError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FatBlockError::Device(msg) => write!(f, "device error: {}", msg),
            FatBlockError::UnexpectedEof => write!(f, "unexpected eof"),
            FatBlockError::WriteZero => write!(f, "write zero"),
        }
    }
}

impl FatIoError for FatBlockError {
    fn is_interrupted(&self) -> bool { false }
    fn new_unexpected_eof_error() -> Self { FatBlockError::UnexpectedEof }
    fn new_write_zero_error() -> Self { FatBlockError::WriteZero }
}

pub struct FatDevice {
    device: Box<dyn BlockDevice>,
    pos: u64,
    bps: u32,
    total_bytes: u64,
}

impl FatDevice {
    pub fn new(device: Box<dyn BlockDevice>) -> Self {
        let bps = device.sector_size();
        let total_bytes = device.total_sectors() * bps as u64;
        Self { device, pos: 0, bps, total_bytes }
    }
}

impl IoBase for FatDevice { type Error = FatBlockError; }

impl Read for FatDevice {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if self.pos >= self.total_bytes { return Ok(0); }
        let end = (self.pos + buf.len() as u64).min(self.total_bytes);
        let len = (end - self.pos) as usize;
        let start_sector = (self.pos / self.bps as u64) as u32;
        let start_off = (self.pos % self.bps as u64) as usize;
        let mut scratch = vec![0u8; self.bps as usize];
        let mut written = 0usize;
        while written < len {
            let sec = start_sector + (written / self.bps as usize) as u32;
            self.device.read_sectors(sec, 1, &mut scratch).map_err(|e| FatBlockError::Device(e))?;
            let off = if sec == start_sector { start_off } else { 0 };
            let avail = (self.bps as usize - off).min(len - written);
            buf[written..written + avail].copy_from_slice(&scratch[off..off + avail]);
            written += avail;
        }
        self.pos += written as u64;
        Ok(written)
    }
}

impl Write for FatDevice {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() { return Ok(0); }
        let sector = (self.pos / self.bps as u64) as u32;
        let offset = (self.pos % self.bps as u64) as usize;
        let write_len = buf.len().min((self.bps as usize).saturating_sub(offset));
        let mut scratch = vec![0u8; self.bps as usize];
        if offset > 0 || write_len < self.bps as usize {
            self.device.read_sectors(sector, 1, &mut scratch).map_err(|e| FatBlockError::Device(e))?;
        }
        scratch[offset..offset + write_len].copy_from_slice(&buf[..write_len]);
        self.device.write_sectors(sector, 1, &scratch).map_err(|e| FatBlockError::Device(e))?;
        self.pos += write_len as u64;
        Ok(write_len)
    }
    fn flush(&mut self) -> Result<(), Self::Error> { Ok(()) }
}

impl Seek for FatDevice {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        let new_pos = match pos {
            SeekFrom::Start(off) => off,
            SeekFrom::End(off) => {
                if off >= 0 { self.total_bytes.saturating_add_signed(off) }
                else { self.total_bytes.saturating_sub((-off) as u64) }
            }
            SeekFrom::Current(off) => {
                if off >= 0 { self.pos.saturating_add_signed(off) }
                else { self.pos.saturating_sub((-off) as u64) }
            }
        };
        self.pos = new_pos.min(self.total_bytes);
        Ok(self.pos)
    }
}

pub fn is_exfat(boot: &[u8; 512]) -> bool {
    &boot[3..11] == b"EXFAT   "
}

// ── FatFileSystem ────────────────────────────────────────────

pub struct FatFileSystem {
    inner: FatType,
    next_fd: u32,
    handles: Vec<(u32, String)>,
}

impl FatFileSystem {
    pub fn from_device(mut device: Box<dyn BlockDevice>) -> Result<Self, FsError> {
        let lba = find_fat_partition(&mut *device)?;
        if lba > 0 {
            let wrapped = PartitionBlockDevice { inner: device, offset: lba };
            let cached = BlockCache::new(wrapped, 64);
            return Self::new(Box::new(cached));
        }
        let cached = BlockCache::new(device, 64);
        Self::new(Box::new(cached))
    }

    pub fn new(device: Box<dyn BlockDevice>) -> Result<Self, FsError> {
        let fat_dev = FatDevice::new(device);
        let opts = fatfs::FsOptions::new();
        let inner = FatType::new(fat_dev, opts)
            .map_err(|e| { klog_fmt!("FAT: fatfs init error: {:?}\n", e); FsError::InvalidInput })?;
        Ok(Self { inner, next_fd: 1, handles: Vec::new() })
    }

    fn map_err<T>(r: Result<T, FatError>) -> Result<T, FsError> {
        r.map_err(|e| match e {
            fatfs::Error::Io(FatBlockError::Device(_)) => FsError::InvalidInput,
            fatfs::Error::NotFound => FsError::FileNotFound,
            fatfs::Error::AlreadyExists => FsError::FileExists,
            fatfs::Error::NotEnoughSpace => FsError::DiskFull,
            fatfs::Error::CorruptedFileSystem => FsError::InvalidInput,
            _ => FsError::InvalidInput,
        })
    }

    fn open_dir<'a>(&'a mut self, path: &str) -> Result<FatDir<'a>, FsError> {
        let path = path.trim_matches('/');
        if path.is_empty() { Ok(self.inner.root_dir()) }
        else { Self::map_err(self.inner.root_dir().open_dir(path)) }
    }

    fn open_file<'a>(&'a mut self, path: &str) -> Result<FatFile<'a>, FsError> {
        let path = path.trim_matches('/');
        Self::map_err(self.inner.root_dir().open_file(path))
    }

    fn create_file<'a>(&'a mut self, path: &str) -> Result<FatFile<'a>, FsError> {
        let path = path.trim_matches('/');
        let (parent, name) = match path.rfind('/') {
            Some(pos) => (&path[..pos], &path[pos + 1..]),
            None => ("", path),
        };
        let dir = self.open_dir(parent)?;
        Self::map_err(dir.create_file(name))
    }
}

impl FileSystem for FatFileSystem {
    fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor> {
        let _ = self.open_file(path).ok()?;
        let fd = self.next_fd;
        self.next_fd += 1;
        self.handles.push((fd, String::from(path)));
        Some(FileDescriptor { fd, ino: 0, offset: 0, flags })
    }

    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        let path = self.handles.iter().find(|h| h.0 == fd).map(|h| h.1.clone()).ok_or(FsError::InvalidFileDescriptor)?;
        let mut file = self.open_file(&path)?;
        Self::map_err(file.read(buf))
    }

    fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, FsError> {
        let path = self.handles.iter().find(|h| h.0 == fd).map(|h| h.1.clone()).ok_or(FsError::InvalidFileDescriptor)?;
        let mut file = self.open_file(&path)?;
        Self::map_err(file.write(data))
    }

    fn close(&mut self, fd: u32) -> Result<(), FsError> {
        let pos = self.handles.iter().position(|h| h.0 == fd).ok_or(FsError::InvalidFileDescriptor)?;
        self.handles.remove(pos);
        Ok(())
    }

    fn seek(&mut self, fd: u32, new_pos: usize) -> Result<(), FsError> {
        let path = self.handles.iter().find(|h| h.0 == fd).map(|h| h.1.clone()).ok_or(FsError::InvalidFileDescriptor)?;
        let mut file = self.open_file(&path)?;
        Self::map_err(file.seek(fatfs::SeekFrom::Start(new_pos as u64)))?;
        Ok(())
    }

    fn create(&mut self, path: &str, _kind: InodeType) -> Option<u64> {
        let _file = self.create_file(path).ok()?;
        Some(0)
    }

    fn mkdir(&mut self, path: &str) -> Result<(), FsError> {
        let path = path.trim_matches('/');
        if path.is_empty() { return Err(FsError::InvalidInput); }
        let (parent, name) = match path.rfind('/') {
            Some(pos) => (&path[..pos], &path[pos + 1..]),
            None => ("", path),
        };
        let dir = self.open_dir(parent)?;
        dir.create_dir(name).map(|_| ()).map_err(|e| match e {
            fatfs::Error::AlreadyExists => FsError::FileExists,
            _ => FsError::InvalidInput,
        })
    }

    fn unlink(&mut self, path: &str) -> Result<(), FsError> {
        let path = path.trim_matches('/');
        if path.is_empty() { return Err(FsError::InvalidInput); }
        let (parent, name) = match path.rfind('/') {
            Some(pos) => (&path[..pos], &path[pos + 1..]),
            None => ("", path),
        };
        let dir = self.open_dir(parent)?;
        Self::map_err(dir.remove(name))
    }

    fn readdir(&mut self, path: &str) -> Result<Vec<VNode>, FsError> {
        let dir = self.open_dir(path)?;
        let mut result = Vec::new();
        for entry in dir.iter() {
            let e = Self::map_err(entry)?;
            let name: String = e.file_name();
            let is_dir = e.is_dir();
            let size = e.len();
            result.push(VNode { name, size, is_dir });
        }
        Ok(result)
    }

    fn exists(&mut self, path: &str) -> bool {
        self.open_file(path).is_ok() || self.open_dir(path).is_ok()
    }
}
