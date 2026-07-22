use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::mem;
use core::pin::Pin;

use hadris_fat::FatError;
use hadris_fat::exfat::{
    ExFatBootSector, ExFatDir, ExFatFileEntry, ExFatFileReader, ExFatFileWriter, ExFatFs,
    ExFatInfo, ExFatTable,
};
use hadris_fat::sync::{Error as IoError, ErrorKind, Read, Seek, SeekFrom, Write};
use spin::Mutex;

use crate::block::{BlockDevice, BlockError};
use crate::fs::FsError;
use crate::vfs::{FileDescriptor, FileSystem, FileSystemCapabilities, InodeType, VNode};

const SECTOR_SIZE: usize = 512;
const ROOT_SCAN_SECTORS: usize = 256;
const METADATA_CACHE_SECTORS: usize = ROOT_SCAN_SECTORS + 16;

type MetadataCache = BTreeMap<u32, [u8; SECTOR_SIZE]>;

pub fn is_exfat(boot_sector: &[u8; SECTOR_SIZE]) -> bool {
    &boot_sector[3..11] == b"EXFAT   "
}

struct ExFatDevice {
    inner: Arc<Mutex<Box<dyn BlockDevice>>>,
    cache: Arc<Mutex<MetadataCache>>,
    position: u64,
    size: u64,
}

impl ExFatDevice {
    fn error(kind: ErrorKind, message: &'static str) -> IoError {
        IoError::new_static(kind, message)
    }

    fn block_error(error: BlockError) -> IoError {
        match error {
            BlockError::BufferTooSmall { .. } | BlockError::LbaOverflow => {
                Self::error(ErrorKind::InvalidInput, "invalid block I/O request")
            }
            BlockError::SectorNotFound => {
                Self::error(ErrorKind::NotFound, "block sector not found")
            }
            BlockError::Device => Self::error(ErrorKind::Other, "block device I/O failed"),
        }
    }

    fn lba(position: u64) -> Result<u32, IoError> {
        u32::try_from(position / SECTOR_SIZE as u64)
            .map_err(|_| Self::error(ErrorKind::InvalidInput, "exFAT LBA overflow"))
    }

    fn read_cached(
        &self,
        device: &mut dyn BlockDevice,
        lba: u32,
        count: u16,
        buf: &mut [u8],
    ) -> Result<(), IoError> {
        let expected_len = usize::from(count) * SECTOR_SIZE;
        let buf = buf
            .get_mut(..expected_len)
            .ok_or_else(|| Self::error(ErrorKind::InvalidInput, "exFAT read buffer too small"))?;
        if count == 1 {
            if let Some(sector) = self.cache.lock().get(&lba) {
                buf[..SECTOR_SIZE].copy_from_slice(sector);
                return Ok(());
            }
        }
        device
            .read_sectors(lba as u64, count, buf)
            .map_err(Self::block_error)?;
        if count == 1 {
            let mut cache = self.cache.lock();
            if cache.len() < METADATA_CACHE_SECTORS {
                let mut sector = [0; SECTOR_SIZE];
                sector.copy_from_slice(&buf[..SECTOR_SIZE]);
                cache.insert(lba, sector);
            }
        }
        Ok(())
    }

    fn write_cached(
        &self,
        device: &mut dyn BlockDevice,
        lba: u32,
        count: u16,
        buf: &[u8],
    ) -> Result<(), IoError> {
        let expected_len = usize::from(count) * SECTOR_SIZE;
        let buf = buf
            .get(..expected_len)
            .ok_or_else(|| Self::error(ErrorKind::InvalidInput, "exFAT write buffer too small"))?;
        device
            .write_sectors(lba as u64, count, buf)
            .map_err(Self::block_error)?;
        let mut cache = self.cache.lock();
        for (index, sector) in buf
            .chunks_exact(SECTOR_SIZE)
            .take(usize::from(count))
            .enumerate()
        {
            if let Some(cached) = lba
                .checked_add(index as u32)
                .and_then(|sector_lba| cache.get_mut(&sector_lba))
            {
                cached.copy_from_slice(sector);
            }
        }
        Ok(())
    }
}

impl Read for ExFatDevice {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        let len = usize::try_from(
            self.size
                .saturating_sub(self.position)
                .min(buf.len() as u64),
        )
        .unwrap_or(buf.len());
        let mut done = 0;
        let mut sector = [0; SECTOR_SIZE];
        let mut device = self.inner.lock();

        while done < len {
            let position = self.position + done as u64;
            let offset = position as usize % SECTOR_SIZE;
            let remaining = len - done;
            if offset == 0 && remaining >= SECTOR_SIZE {
                let count = (remaining / SECTOR_SIZE).min(u16::MAX as usize) as u16;
                let bytes = count as usize * SECTOR_SIZE;
                self.read_cached(
                    &mut **device,
                    Self::lba(position)?,
                    count,
                    &mut buf[done..done + bytes],
                )?;
                done += bytes;
                continue;
            }
            self.read_cached(&mut **device, Self::lba(position)?, 1, &mut sector)?;
            let bytes = (SECTOR_SIZE - offset).min(remaining);
            buf[done..done + bytes].copy_from_slice(&sector[offset..offset + bytes]);
            done += bytes;
        }
        self.position += done as u64;
        Ok(done)
    }
}

impl Write for ExFatDevice {
    fn write(&mut self, buf: &[u8]) -> Result<usize, IoError> {
        let len = usize::try_from(
            self.size
                .saturating_sub(self.position)
                .min(buf.len() as u64),
        )
        .unwrap_or(buf.len());
        let mut done = 0;
        let mut sector = [0; SECTOR_SIZE];
        let mut device = self.inner.lock();

        while done < len {
            let position = self.position + done as u64;
            let offset = position as usize % SECTOR_SIZE;
            let remaining = len - done;
            if offset == 0 && remaining >= SECTOR_SIZE {
                let count = (remaining / SECTOR_SIZE).min(u16::MAX as usize) as u16;
                let bytes = count as usize * SECTOR_SIZE;
                self.write_cached(
                    &mut **device,
                    Self::lba(position)?,
                    count,
                    &buf[done..done + bytes],
                )?;
                done += bytes;
                continue;
            }
            self.read_cached(&mut **device, Self::lba(position)?, 1, &mut sector)?;
            let bytes = (SECTOR_SIZE - offset).min(remaining);
            sector[offset..offset + bytes].copy_from_slice(&buf[done..done + bytes]);
            self.write_cached(&mut **device, Self::lba(position)?, 1, &sector)?;
            done += bytes;
        }
        self.position += done as u64;
        Ok(done)
    }

    fn flush(&mut self) -> Result<(), IoError> {
        Ok(())
    }
}

impl Seek for ExFatDevice {
    fn seek(&mut self, position: SeekFrom) -> Result<u64, IoError> {
        let position = match position {
            SeekFrom::Start(offset) => offset as i128,
            SeekFrom::End(offset) => self.size as i128 + offset as i128,
            SeekFrom::Current(offset) => self.position as i128 + offset as i128,
        };
        if !(0..=self.size as i128).contains(&position) {
            return Err(Self::error(
                ErrorKind::InvalidInput,
                "exFAT seek outside device",
            ));
        }
        self.position = position as u64;
        Ok(self.position)
    }
}

type ExFatInner = ExFatFs<ExFatDevice>;
type Writer = ExFatFileWriter<'static, ExFatDevice>;
type Reader = ExFatFileReader<'static, ExFatDevice>;

struct Handle {
    fd: u32,
    entry: ExFatFileEntry,
    reader: Option<Reader>,
    offset: u64,
    writer: Option<Writer>,
}

// Handles must be finalized before inner is dropped because writers
// contain a stable reference to the pinned filesystem allocation.
pub struct ExFatFileSystem {
    handles: Vec<Handle>,
    inner: Pin<Box<ExFatInner>>,
    device: Arc<Mutex<Box<dyn BlockDevice>>>,
    cache: Arc<Mutex<MetadataCache>>,
    device_size: u64,
    next_fd: u32,
    root_cache: Option<Vec<VNode>>,
    dir_cache: BTreeMap<String, Vec<VNode>>,
}

impl ExFatFileSystem {
    pub fn new(
        device: Box<dyn BlockDevice>,
    ) -> Result<Self, (FsError, Option<Box<dyn BlockDevice>>)> {
        if device.sector_size() as usize != SECTOR_SIZE {
            return Err((FsError::NotSupported, Some(device)));
        }
        let size = match device.total_sectors().checked_mul(SECTOR_SIZE as u64) {
            Some(size) => size,
            None => return Err((FsError::InvalidInput, Some(device))),
        };
        let shared = Arc::new(Mutex::new(device));
        let cache = Arc::new(Mutex::new(MetadataCache::new()));
        let mut adapter = ExFatDevice {
            inner: Arc::clone(&shared),
            cache: Arc::clone(&cache),
            position: 0,
            size,
        };
        let prefetch = ExFatBootSector::read(&mut adapter)
            .map_err(Self::map_error)
            .and_then(|boot| {
                cache.lock().clear();
                Self::scan_root(&mut adapter, boot.info(), |sector| {
                    sector.chunks_exact(32).any(|entry| entry[0] == 0)
                })
            });
        if let Err(error) = prefetch {
            log::info!("exFAT: root prefetch error: {:?}", error);
            drop(adapter);
            let device = Arc::try_unwrap(shared).ok().map(Mutex::into_inner);
            return Err((error, device));
        }
        let inner = match ExFatInner::open(adapter) {
            Ok(inner) => inner,
            Err(error) => {
                log::info!("exFAT: mount error: {:?}", error);
                let device = Arc::try_unwrap(shared).ok().map(Mutex::into_inner);
                return Err((Self::map_error(error), device));
            }
        };
        let info = inner.info();
        log::info!(
            "exFAT: volume mounted ({} bytes/sector, {} sectors/cluster)",
            info.bytes_per_sector,
            info.sectors_per_cluster
        );
        Ok(Self {
            handles: Vec::new(),
            inner: Box::pin(inner),
            device: shared,
            cache,
            device_size: size,
            next_fd: 1,
            root_cache: None,
            dir_cache: BTreeMap::new(),
        })
    }

    fn fs(&self) -> &ExFatInner {
        self.inner.as_ref().get_ref()
    }

    fn map_error(error: FatError) -> FsError {
        match error {
            FatError::EntryNotFound => FsError::FileNotFound,
            FatError::AlreadyExists => FsError::FileExists,
            FatError::NoFreeSpace => FsError::DiskFull,
            FatError::DirectoryNotEmpty => FsError::DirectoryNotEmpty,
            FatError::NotADirectory => FsError::NotADirectory,
            FatError::NotAFile => FsError::IsADirectory,
            FatError::InvalidPath | FatError::InvalidFilename => FsError::InvalidPath,
            FatError::Io(error) => Self::map_io_error(error),
            FatError::IoContext { source, .. } => Self::map_io_error(source),
            _ => FsError::InvalidInput,
        }
    }

    fn map_io_error(error: IoError) -> FsError {
        match error.kind() {
            ErrorKind::NotFound => FsError::FileNotFound,
            ErrorKind::AlreadyExists => FsError::FileExists,
            ErrorKind::PermissionDenied => FsError::PermissionDenied,
            ErrorKind::OutOfMemory | ErrorKind::WriteZero => FsError::DiskFull,
            _ => FsError::InvalidInput,
        }
    }

    fn scan_root(
        device: &mut ExFatDevice,
        info: &ExFatInfo,
        mut visit: impl FnMut(&[u8; SECTOR_SIZE]) -> bool,
    ) -> Result<(), FsError> {
        let cluster_size = info.bytes_per_cluster;
        if cluster_size < SECTOR_SIZE || !cluster_size.is_multiple_of(SECTOR_SIZE) {
            return Err(FsError::InvalidInput);
        }
        let fat = ExFatTable::new(info);
        let mut cluster = info.root_cluster;
        let mut visited = BTreeSet::new();
        let mut sector = [0; SECTOR_SIZE];
        let mut sectors = 0;

        loop {
            if !visited.insert(cluster) {
                return Err(FsError::InvalidInput);
            }
            device
                .seek(SeekFrom::Start(info.cluster_to_offset(cluster)))
                .map_err(Self::map_io_error)?;
            for _ in 0..cluster_size / SECTOR_SIZE {
                if sectors == ROOT_SCAN_SECTORS {
                    return Err(FsError::InvalidInput);
                }
                device.read_exact(&mut sector).map_err(Self::map_io_error)?;
                sectors += 1;
                if visit(&sector) {
                    return Ok(());
                }
            }
            match fat.next_cluster(device, cluster).map_err(Self::map_error)? {
                Some(next) => cluster = next,
                None => return Ok(()),
            }
        }
    }

    fn split_path(path: &str) -> Result<(&str, &str), FsError> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return Err(FsError::InvalidPath);
        }
        Ok(path.rsplit_once('/').unwrap_or(("", path)))
    }

    fn directory(&self, path: &str) -> Result<ExFatDir<'_, ExFatDevice>, FsError> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            Ok(self.fs().root_dir())
        } else {
            self.fs().open_dir(path).map_err(Self::map_error)
        }
    }

    fn create_file(&self, path: &str) -> Result<(), FsError> {
        let (parent, name) = Self::split_path(path)?;
        let parent = self.directory(parent)?;
        self.fs()
            .create_file(&parent, name)
            .map(|_| ())
            .map_err(Self::map_error)
    }

    fn create_directory(&self, path: &str) -> Result<(), FsError> {
        let (parent, name) = Self::split_path(path)?;
        let parent = self.directory(parent)?;
        self.fs()
            .create_dir(&parent, name)
            .map(|_| ())
            .map_err(Self::map_error)
    }

    fn next_descriptor(&mut self) -> u32 {
        loop {
            let fd = self.next_fd.max(1);
            self.next_fd = fd.wrapping_add(1).max(1);
            if self.handles.iter().all(|handle| handle.fd != fd) {
                return fd;
            }
        }
    }

    fn handle_index(&self, fd: u32) -> Result<usize, FsError> {
        self.handles
            .iter()
            .position(|handle| handle.fd == fd)
            .ok_or(FsError::InvalidFileDescriptor)
    }

    fn open_writer_from_entry(&self, entry: &ExFatFileEntry) -> Result<Writer, FsError> {
        let mut entry = entry.clone();
        if entry.first_cluster == 0 && entry.data_length == 0 {
            entry.no_fat_chain = true;
        }
        let writer = self.fs().write_file(&entry).map_err(Self::map_error)?;
        // SAFETY: inner is pinned in a Box and is therefore never moved. Every
        // writer is removed/finalized before inner is dropped (see Drop).
        Ok(unsafe { mem::transmute::<ExFatFileWriter<'_, ExFatDevice>, Writer>(writer) })
    }

    fn invalidate_dir_cache(&mut self) {
        self.root_cache = None;
        self.dir_cache.clear();
    }

    fn root_entries(&self) -> Result<Vec<VNode>, FsError> {
        let info = self.fs().info();
        let mut device = ExFatDevice {
            inner: Arc::clone(&self.device),
            cache: Arc::clone(&self.cache),
            position: 0,
            size: self.device_size,
        };
        let mut pending = Vec::with_capacity(19);
        let mut entries = Vec::new();
        Self::scan_root(&mut device, info, |sector| {
            sector.chunks_exact(32).any(|chunk| {
                let mut raw = [0; 32];
                raw.copy_from_slice(chunk);
                Self::consume_entry(raw, &mut pending, &mut entries)
            })
        })?;
        Ok(entries)
    }

    fn consume_entry(raw: [u8; 32], pending: &mut Vec<[u8; 32]>, entries: &mut Vec<VNode>) -> bool {
        if raw[0] == 0 {
            return true;
        }
        if pending.is_empty() {
            if raw[0] == 0x85 && (2..=18).contains(&raw[1]) {
                pending.push(raw);
            }
            return false;
        }

        pending.push(raw);
        let expected = pending[0][1] as usize + 1;
        if pending.len() < expected {
            return false;
        }
        if let Some(entry) = Self::parse_entry(pending) {
            entries.push(entry);
        }
        pending.clear();
        false
    }

    fn parse_entry(raw: &[[u8; 32]]) -> Option<VNode> {
        let stream = raw.get(1)?;
        if stream[0] != 0xC0 {
            return None;
        }
        let name_len = stream[3] as usize;
        if name_len == 0 {
            return None;
        }
        let mut name = Vec::with_capacity(name_len);
        for entry in raw.get(2..)? {
            if entry[0] != 0xC1 {
                return None;
            }
            for bytes in entry[2..].chunks_exact(2) {
                if name.len() == name_len {
                    break;
                }
                name.push(u16::from_le_bytes([bytes[0], bytes[1]]));
            }
        }
        (name.len() == name_len).then(|| VNode {
            size: u64::from_le_bytes(stream[8..16].try_into().unwrap()),
            is_dir: u16::from_le_bytes(raw[0][4..6].try_into().unwrap()) & 0x10 != 0,
            name: String::from_utf16_lossy(&name),
        })
    }
}

impl Drop for ExFatFileSystem {
    fn drop(&mut self) {
        while let Some(mut handle) = self.handles.pop() {
            if let Some(writer) = handle.writer.take() {
                let _ = writer.finish();
            }
        }
    }
}

impl FileSystem for ExFatFileSystem {
    fn capabilities(&self) -> FileSystemCapabilities {
        FileSystemCapabilities::new(false, true, true, false, true)
    }

    fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor> {
        let entry = self.fs().open_path(path).ok()?;
        let reader = ExFatFileReader::new(self.fs(), &entry).ok()?;
        // SAFETY: inner is pinned in a Box and is therefore never moved. Every
        // reader is removed/dropped before inner is dropped (see Drop).
        let reader: Reader = unsafe { mem::transmute(reader) };
        let fd = self.next_descriptor();
        self.handles.push(Handle {
            fd,
            entry,
            reader: Some(reader),
            offset: 0,
            writer: None,
        });
        Some(FileDescriptor {
            fd,
            ino: 0,
            offset: 0,
            flags,
        })
    }

    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        let index = self.handle_index(fd)?;
        if self.handles[index].writer.is_some() {
            return Err(FsError::PermissionDenied);
        }
        let handle = &mut self.handles[index];
        let reader = handle
            .reader
            .as_mut()
            .ok_or(FsError::InvalidFileDescriptor)?;
        let read = reader.read(buf).map_err(Self::map_io_error)?;
        handle.offset += read as u64;
        Ok(read)
    }

    fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, FsError> {
        self.invalidate_dir_cache();
        let index = self.handle_index(fd)?;
        if data.is_empty() {
            return Ok(0);
        }
        if self.handles[index].writer.is_none() {
            if self.handles[index].offset != 0 {
                return Err(FsError::InvalidSeek);
            }
            // Refresh the entry to pick up any concurrent writes
            let refreshed_entry = self.handles[index].entry.clone();
            let writer = self.open_writer_from_entry(&refreshed_entry)?;
            self.handles[index].writer = Some(writer);
        }
        let written = self.handles[index]
            .writer
            .as_mut()
            .ok_or(FsError::InvalidInput)?
            .write(data)
            .map_err(Self::map_io_error)?;
        if written == 0 {
            return Err(FsError::DiskFull);
        }
        self.handles[index].offset += written as u64;
        Ok(written)
    }

    fn close(&mut self, fd: u32) -> Result<(), FsError> {
        let index = self.handle_index(fd)?;
        let mut handle = self.handles.remove(index);
        match handle.writer.take() {
            Some(writer) => {
                self.invalidate_dir_cache();
                writer.finish().map_err(Self::map_error)
            }
            None => Ok(()),
        }
    }

    fn seek(&mut self, fd: u32, new_pos: u64) -> Result<(), FsError> {
        let index = self.handle_index(fd)?;
        let handle = &mut self.handles[index];
        if handle.writer.is_some() && new_pos != handle.offset {
            return Err(FsError::InvalidSeek);
        }
        if let Some(reader) = handle.reader.as_mut() {
            reader
                .seek(SeekFrom::Start(new_pos))
                .map_err(Self::map_io_error)?;
        }
        handle.offset = new_pos;
        Ok(())
    }

    fn create(&mut self, path: &str, kind: InodeType) -> Option<u64> {
        self.invalidate_dir_cache();
        match kind {
            InodeType::Directory => self.create_directory(path).ok()?,
            InodeType::File => self.create_file(path).ok()?,
            InodeType::Symlink => return None,
        }
        Some(0)
    }

    fn mkdir(&mut self, path: &str) -> Result<(), FsError> {
        self.invalidate_dir_cache();
        self.create_directory(path)
    }

    fn unlink(&mut self, path: &str) -> Result<(), FsError> {
        self.invalidate_dir_cache();
        let entry = self.fs().open_path(path).map_err(Self::map_error)?;
        self.fs().delete(&entry).map_err(Self::map_error)
    }

    fn readdir(&mut self, path: &str) -> Result<Vec<VNode>, FsError> {
        const MAX_ENTRIES: usize = 4096;
        let trimmed = path.trim_matches('/');
        let cache_key = trimmed.to_lowercase();
        if trimmed.is_empty() {
            if let Some(cached) = self.root_cache.as_ref() {
                return Ok(cached.clone());
            }
        } else if let Some(cached) = self.dir_cache.get(&cache_key) {
            return Ok(cached.clone());
        }
        if trimmed.is_empty() {
            let entries: Vec<_> = self.root_entries()?.into_iter().take(MAX_ENTRIES).collect();
            self.root_cache = Some(entries.clone());
            return Ok(entries);
        }
        let entries: Result<Vec<_>, _> = self
            .directory(path)?
            .entries()
            .take(MAX_ENTRIES)
            .map(|entry| {
                entry
                    .map(|entry| VNode {
                        size: entry.size(),
                        is_dir: entry.is_directory(),
                        name: entry.name,
                    })
                    .map_err(Self::map_error)
            })
            .collect();
        let entries = entries?;
        const MAX_DIR_CACHE_ENTRIES: usize = 256;
        if self.dir_cache.len() >= MAX_DIR_CACHE_ENTRIES {
            self.dir_cache.clear();
        }
        self.dir_cache.insert(cache_key, entries.clone());
        Ok(entries)
    }

    fn exists(&mut self, path: &str) -> bool {
        path.trim_matches('/').is_empty() || self.fs().open_path(path).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use core::sync::atomic::{AtomicUsize, Ordering};

    use hadris_fat::exfat::{ExFatFormatOptions, format_exfat};

    use super::*;

    const IMAGE_SIZE: usize = 16 * 1024 * 1024;

    #[derive(Clone)]
    struct MemoryDevice {
        image: Arc<Mutex<Vec<u8>>>,
        reads: Arc<AtomicUsize>,
        read_calls: Arc<AtomicUsize>,
    }

    impl BlockDevice for MemoryDevice {
        fn read_sectors(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
            let (start, len) = Self::range(lba, count)?;
            if buf.len() < len {
                return Err(BlockError::BufferTooSmall {
                    required: len,
                    provided: buf.len(),
                });
            }
            self.read_calls.fetch_add(1, Ordering::Relaxed);
            self.reads.fetch_add(count as usize, Ordering::Relaxed);
            buf[..len].copy_from_slice(&self.image.lock()[start..start + len]);
            Ok(())
        }

        fn write_sectors(&mut self, lba: u64, count: u16, buf: &[u8]) -> Result<(), BlockError> {
            let (start, len) = Self::range(lba, count)?;
            if buf.len() < len {
                return Err(BlockError::BufferTooSmall {
                    required: len,
                    provided: buf.len(),
                });
            }
            self.image.lock()[start..start + len].copy_from_slice(&buf[..len]);
            Ok(())
        }

        fn sector_size(&self) -> u32 {
            SECTOR_SIZE as u32
        }

        fn total_sectors(&self) -> u64 {
            IMAGE_SIZE as u64 / SECTOR_SIZE as u64
        }
    }

    impl MemoryDevice {
        fn new(image: Arc<Mutex<Vec<u8>>>) -> Self {
            Self {
                image,
                reads: Arc::new(AtomicUsize::new(0)),
                read_calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn range(lba: u64, count: u16) -> Result<(usize, usize), BlockError> {
            let start = lba as usize * SECTOR_SIZE;
            let len = count as usize * SECTOR_SIZE;
            if start.checked_add(len).is_none_or(|end| end > IMAGE_SIZE) {
                return Err(BlockError::LbaOverflow);
            }
            Ok((start, len))
        }
    }

    #[test]
    fn cached_io_rejects_short_sector_buffers() {
        let image = Arc::new(Mutex::new(vec![0; IMAGE_SIZE]));
        let mut device = MemoryDevice::new(image);
        let adapter = ExFatDevice {
            inner: Arc::new(Mutex::new(Box::new(device.clone()))),
            cache: Arc::new(Mutex::new(MetadataCache::new())),
            position: 0,
            size: IMAGE_SIZE as u64,
        };
        let mut read = [0; SECTOR_SIZE - 1];
        assert_eq!(
            adapter
                .read_cached(&mut device, 0, 1, &mut read)
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidInput
        );
        assert_eq!(
            adapter
                .write_cached(&mut device, 0, 1, &read)
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidInput
        );
    }

    #[test]
    fn mounts_and_streams_a_windows_sized_exfat_cluster() {
        let image = Arc::new(Mutex::new(vec![0; IMAGE_SIZE]));
        let block_device = MemoryDevice::new(Arc::clone(&image));
        let shared: Arc<Mutex<Box<dyn BlockDevice>>> =
            Arc::new(Mutex::new(Box::new(block_device.clone())));
        let adapter = ExFatDevice {
            inner: shared,
            cache: Arc::new(Mutex::new(MetadataCache::new())),
            position: 0,
            size: IMAGE_SIZE as u64,
        };
        let options = ExFatFormatOptions::new()
            .with_label("FULLERENE")
            .with_sectors_per_cluster(256);
        drop(format_exfat(adapter, IMAGE_SIZE as u64, &options).unwrap());

        assert_eq!(image.lock()[109], 8);

        let mut fs = match crate::fat::mount_device(Box::new(block_device.clone())) {
            Ok(filesystem) => filesystem,
            Err((error, _)) => panic!("mount failed: {error}"),
        };
        assert_eq!(fs.create("/Bootlog.txt", InodeType::File), Some(0));
        let fd = fs.open("/Bootlog.txt", 0).unwrap().fd;
        let expected: Vec<_> = (0..140_000).map(|index| index as u8).collect();
        for chunk in expected.chunks(4096) {
            assert_eq!(fs.write(fd, chunk).unwrap(), chunk.len());
        }
        fs.close(fd).unwrap();

        let fd = fs.open("/Bootlog.txt", 0).unwrap().fd;
        let mut actual = vec![0; expected.len()];
        let mut offset = 0;
        while offset < actual.len() {
            let end = (offset + 4096).min(actual.len());
            let read = fs
                .read(fd, &mut actual[offset..end])
                .unwrap_or_else(|error| panic!("read failed at {offset}: {error}"));
            if read == 0 {
                break;
            }
            offset += read;
        }
        fs.close(fd).unwrap();
        assert_eq!(offset, expected.len());
        assert_eq!(actual, expected);

        block_device.reads.store(0, Ordering::Relaxed);
        block_device.read_calls.store(0, Ordering::Relaxed);
        let entries = fs.readdir("/").unwrap();
        assert!(entries.iter().any(|entry| entry.name == "Bootlog.txt"));
        assert_eq!(
            block_device.reads.load(Ordering::Relaxed),
            0,
            "root scan repeated media I/O instead of using mount metadata"
        );
        assert_eq!(block_device.read_calls.load(Ordering::Relaxed), 0);

        for index in 0..48 {
            let path = alloc::format!("/entry-{index:02}.txt");
            assert_eq!(fs.create(&path, InodeType::File), Some(0));
        }
        drop(fs);
        let mut fs = match crate::fat::mount_device(Box::new(block_device.clone())) {
            Ok(filesystem) => filesystem,
            Err((error, _)) => panic!("large-root remount failed: {error}"),
        };
        block_device.reads.store(0, Ordering::Relaxed);
        block_device.read_calls.store(0, Ordering::Relaxed);
        let entries = fs.readdir("/").unwrap();
        assert!(entries.iter().any(|entry| entry.name == "entry-47.txt"));
        assert_eq!(block_device.reads.load(Ordering::Relaxed), 0);
        assert_eq!(block_device.read_calls.load(Ordering::Relaxed), 0);

        drop(fs);
        remove_root_end_marker(&mut image.lock());
        let mut fs = match crate::fat::mount_device(Box::new(block_device.clone())) {
            Ok(filesystem) => filesystem,
            Err((error, _)) => panic!("remount failed: {error}"),
        };
        block_device.reads.store(0, Ordering::Relaxed);
        block_device.read_calls.store(0, Ordering::Relaxed);
        let entries = fs.readdir("/").unwrap();
        assert!(entries.iter().any(|entry| entry.name == "Bootlog.txt"));
        assert!(
            block_device.reads.load(Ordering::Relaxed) <= 256,
            "root scan escaped its FAT chain"
        );
        assert_eq!(
            block_device.read_calls.load(Ordering::Relaxed),
            block_device.reads.load(Ordering::Relaxed),
            "root scan issued a multi-sector request"
        );
    }

    fn remove_root_end_marker(image: &mut [u8]) {
        let le_u32 = |offset| u32::from_le_bytes(image[offset..offset + 4].try_into().unwrap());
        let sector_size = 1usize << image[108];
        let sectors_per_cluster = 1usize << image[109];
        let cluster_heap = le_u32(88) as usize;
        let root_cluster = le_u32(96) as usize;
        let start = (cluster_heap + (root_cluster - 2) * sectors_per_cluster) * sector_size;
        let end = start + sectors_per_cluster * sector_size;
        for offset in (start..end).step_by(32) {
            if image[offset] == 0 {
                image[offset] = 0x05;
            }
        }
    }

    // ── I/O benchmark helpers ─────────────────────────────────

    /// Kernel's vfs_read pattern: open → read(4 KiB) → grow Vec → … → close.
    fn read_vfs(
        fs: &mut dyn crate::vfs::FileSystem,
        path: &str,
    ) -> Result<Vec<u8>, crate::FsError> {
        let fd = fs.open(path, 0).ok_or(crate::FsError::FileNotFound)?;
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            match fs.read(fd.fd, &mut tmp) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                Err(e) => {
                    let _ = fs.close(fd.fd);
                    return Err(e);
                }
            }
        }
        let _ = fs.close(fd.fd);
        Ok(buf)
    }

    /// Open-per-chunk pattern: open → seek → read(4 KiB) → close × N.
    fn read_per_chunk(
        fs: &mut dyn crate::vfs::FileSystem,
        path: &str,
        total: usize,
    ) -> Result<Vec<u8>, crate::FsError> {
        let mut buf = Vec::with_capacity(total);
        let mut off = 0u64;
        while off < total as u64 {
            let fd = fs.open(path, 0).ok_or(crate::FsError::FileNotFound)?;
            let seek_result = fs.seek(fd.fd, off);
            if let Err(e) = seek_result {
                let _ = fs.close(fd.fd);
                return Err(e);
            }
            let mut tmp = [0u8; 4096];
            let read_result = fs.read(fd.fd, &mut tmp);
            let _ = fs.close(fd.fd);
            let n = read_result?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            off += n as u64;
        }
        Ok(buf)
    }

    #[test]
    fn concurrent_write_regression() {
        let image = Arc::new(Mutex::new(vec![0; IMAGE_SIZE]));
        let block_device = MemoryDevice::new(Arc::clone(&image));
        let shared: Arc<Mutex<Box<dyn BlockDevice>>> =
            Arc::new(Mutex::new(Box::new(block_device.clone())));
        let adapter = ExFatDevice {
            inner: shared,
            cache: Arc::new(Mutex::new(MetadataCache::new())),
            position: 0,
            size: IMAGE_SIZE as u64,
        };
        let options = ExFatFormatOptions::new()
            .with_label("CONCUR")
            .with_sectors_per_cluster(256);
        drop(format_exfat(adapter, IMAGE_SIZE as u64, &options).unwrap());

        let mut fs = match crate::fat::mount_device(Box::new(block_device.clone())) {
            Ok(filesystem) => filesystem,
            Err((error, _)) => panic!("mount failed: {error}"),
        };

        // Create a file and write initial data
        assert_eq!(fs.create("/test.dat", InodeType::File), Some(0));
        let fd_a = fs.open("/test.dat", 0).unwrap().fd;
        let data_a = b"first write from A";
        assert_eq!(fs.write(fd_a, data_a).unwrap(), data_a.len());
        fs.close(fd_a).unwrap();

        // Open two handles
        let fd_a = fs.open("/test.dat", 0).unwrap().fd;
        let fd_b = fs.open("/test.dat", 0).unwrap().fd;

        // A writes and closes
        let data_a2 = b"second write from A";
        assert_eq!(fs.write(fd_a, data_a2).unwrap(), data_a2.len());
        fs.close(fd_a).unwrap();

        // B writes and closes (should see A's updated length/clusters)
        let data_b = b"write from B after A";
        assert_eq!(fs.write(fd_b, data_b).unwrap(), data_b.len());
        fs.close(fd_b).unwrap();

        // Verify final content matches B's write
        let fd = fs.open("/test.dat", 0).unwrap().fd;
        let mut buf = vec![0; data_b.len()];
        assert_eq!(fs.read(fd, &mut buf).unwrap(), data_b.len());
        assert_eq!(&buf[..], data_b);
        fs.close(fd).unwrap();
    }

    #[test]
    #[ignore = "manual I/O throughput benchmark (use --include-ignored)"]
    fn bench_file_read_throughput() {
        extern crate std;

        const IMG: usize = 64 * 1024 * 1024; // 64 MiB image
        let image = Arc::new(spin::Mutex::new(vec![0u8; IMG]));
        let block_device = MemoryDevice::new(Arc::clone(&image));

        // Format
        let shared: Arc<Mutex<Box<dyn BlockDevice>>> =
            Arc::new(Mutex::new(Box::new(block_device.clone())));
        let adapter = ExFatDevice {
            inner: shared,
            cache: Arc::new(Mutex::new(MetadataCache::new())),
            position: 0,
            size: IMG as u64,
        };
        let opts = hadris_fat::exfat::ExFatFormatOptions::new()
            .with_label("BENCH")
            .with_sectors_per_cluster(256);
        hadris_fat::exfat::format_exfat(adapter, IMG as u64, &opts).expect("format should succeed");

        let mut fs = match crate::fat::mount_device(Box::new(block_device.clone())) {
            Ok(fs) => fs,
            Err((e, _)) => {
                std::eprintln!("BENCH mount failed: {:?}", e);
                return;
            }
        };

        let sizes: &[usize] = &[100, 1024, 5 * 1024, 8 * 1024];

        for &kb in sizes {
            let nbytes = kb * 1024;
            let path = alloc::format!("/f-{}.dat", kb);
            let data: Vec<u8> = (0..nbytes).map(|i| (i % 251) as u8).collect();

            // Write
            let _ = fs.create(&path, crate::vfs::InodeType::File);
            let fd = fs.open(&path, 0).expect("open");
            let mut written = 0usize;
            for chunk in data.chunks(65536) {
                let n = fs.write(fd.fd, chunk).unwrap_or(0);
                written += n;
            }
            fs.close(fd.fd).unwrap();
            if written < nbytes {
                std::eprintln!(
                    "BENCH {:>4}K: write incomplete ({} / {} bytes), skipping",
                    kb,
                    written,
                    nbytes,
                );
                continue;
            }

            // VFS-style read
            block_device.reads.store(0, Ordering::Relaxed);
            block_device.read_calls.store(0, Ordering::Relaxed);
            let t0 = std::time::Instant::now();
            let r1 = read_vfs(&mut *fs, &path).expect("vfs");
            let d1 = t0.elapsed();
            let s1 = block_device.reads.load(Ordering::Relaxed);
            let c1 = block_device.read_calls.load(Ordering::Relaxed);

            // Per-chunk read
            block_device.reads.store(0, Ordering::Relaxed);
            block_device.read_calls.store(0, Ordering::Relaxed);
            let t0 = std::time::Instant::now();
            let r2 = read_per_chunk(&mut *fs, &path, r1.len()).expect("chunk");
            let d2 = t0.elapsed();
            let s2 = block_device.reads.load(Ordering::Relaxed);
            let c2 = block_device.read_calls.load(Ordering::Relaxed);

            assert_eq!(r1.len(), r2.len(), "VFS and per-chunk read sizes differ");

            let ratio = d2.as_secs_f64() / d1.as_secs_f64().max(1e-9);
            std::eprintln!(
                "BENCH {:>4}K | VFS: {:>8?} (sec={:>5}, call={:>4}) | \
                 reopen: {:>8?} (sec={:>5}, call={:>4}) | reopen/VFS = {:.1}x",
                kb,
                d1,
                s1,
                c1,
                d2,
                s2,
                c2,
                ratio,
            );

            // Cleanup
            let fd = fs.open(&path, 0).unwrap().fd;
            fs.close(fd).unwrap();
        }
    }
}
