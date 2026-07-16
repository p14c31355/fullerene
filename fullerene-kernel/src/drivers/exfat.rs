//! exFAT filesystem adapter for the kernel VFS.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::mem;
use core::pin::Pin;

use genome::fs::FsError;
use hadris_fat::FatError;
use hadris_fat::exfat::{
    ExFatBootSector, ExFatDir, ExFatFileWriter, ExFatFs, ExFatInfo, ExFatTable,
};
use hadris_fat::sync::{Error as IoError, ErrorKind, Read, Seek, SeekFrom, Write};
use spin::Mutex;

use crate::contexts::vfs::{FileDescriptor, FileSystem, InodeType, VNode};
use crate::drivers::fat::BlockDevice;
use crate::klog_fmt;

const SECTOR_SIZE: usize = 512;
const ROOT_SCAN_SECTORS: usize = 256;
const METADATA_CACHE_SECTORS: usize = ROOT_SCAN_SECTORS + 16;

type MetadataCache = BTreeMap<u32, [u8; SECTOR_SIZE]>;

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
            .read_sectors(lba, count, buf)
            .map_err(|message| Self::error(ErrorKind::Other, message))?;
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
            .write_sectors(lba, count, buf)
            .map_err(|message| Self::error(ErrorKind::Other, message))?;
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

struct Handle {
    fd: u32,
    path: String,
    offset: u64,
    writer: Option<Writer>,
}

pub struct ExFatFileSystem {
    // Handles must be finalized before `inner` is dropped because writers
    // contain a stable reference to the pinned filesystem allocation.
    handles: Vec<Handle>,
    inner: Pin<Box<ExFatInner>>,
    device: Arc<Mutex<Box<dyn BlockDevice>>>,
    cache: Arc<Mutex<MetadataCache>>,
    device_size: u64,
    next_fd: u32,
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
            klog_fmt!("exFAT: root prefetch error: {:?}\n", error);
            drop(adapter);
            let device = Arc::try_unwrap(shared).ok().map(Mutex::into_inner);
            return Err((error, device));
        }
        let inner = match ExFatInner::open(adapter) {
            Ok(inner) => inner,
            Err(error) => {
                klog_fmt!("exFAT: mount error: {:?}\n", error);
                let device = Arc::try_unwrap(shared).ok().map(Mutex::into_inner);
                return Err((Self::map_error(error), device));
            }
        };
        let info = inner.info();
        klog_fmt!(
            "exFAT: volume mounted ({} bytes/sector, {} sectors/cluster)\n",
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

    fn open_writer(&self, path: &str) -> Result<Writer, FsError> {
        let mut entry = self.fs().open_path(path).map_err(Self::map_error)?;
        // Empty exFAT streams have no allocation flags on disk. The writer
        // still has to start in contiguous mode so a second cluster remains
        // addressable without a FAT chain.
        if entry.first_cluster == 0 && entry.data_length == 0 {
            entry.no_fat_chain = true;
        }
        let writer = self.fs().write_file(&entry).map_err(Self::map_error)?;
        // SAFETY: `inner` is pinned in a Box and is therefore never moved. Every
        // writer is removed/finalized before `inner` is dropped (see `Drop`).
        Ok(unsafe { mem::transmute::<ExFatFileWriter<'_, ExFatDevice>, Writer>(writer) })
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
    fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor> {
        self.fs().open_file(path).ok()?;
        let fd = self.next_descriptor();
        self.handles.push(Handle {
            fd,
            path: String::from(path),
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
        let offset = self.handles[index].offset;
        let read = {
            let mut file = self
                .fs()
                .open_file(&self.handles[index].path)
                .map_err(Self::map_error)?;
            if offset != 0 {
                file.seek(SeekFrom::Start(offset))
                    .map_err(Self::map_io_error)?;
            }
            file.read(buf).map_err(Self::map_io_error)?
        };
        self.handles[index].offset += read as u64;
        Ok(read)
    }

    fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, FsError> {
        let index = self.handle_index(fd)?;
        if data.is_empty() {
            return Ok(0);
        }
        if self.handles[index].writer.is_none() {
            if self.handles[index].offset != 0 {
                return Err(FsError::InvalidSeek);
            }
            let writer = self.open_writer(&self.handles[index].path)?;
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
            Some(writer) => writer.finish().map_err(Self::map_error),
            None => Ok(()),
        }
    }

    fn seek(&mut self, fd: u32, new_pos: usize) -> Result<(), FsError> {
        let index = self.handle_index(fd)?;
        let handle = &mut self.handles[index];
        if handle.writer.is_some() && new_pos as u64 != handle.offset {
            return Err(FsError::InvalidSeek);
        }
        handle.offset = new_pos as u64;
        Ok(())
    }

    fn create(&mut self, path: &str, kind: InodeType) -> Option<u64> {
        match kind {
            InodeType::Directory => self.create_directory(path).ok()?,
            InodeType::File => self.create_file(path).ok()?,
            InodeType::Symlink => return None,
        }
        Some(0)
    }

    fn mkdir(&mut self, path: &str) -> Result<(), FsError> {
        self.create_directory(path)
    }

    fn unlink(&mut self, path: &str) -> Result<(), FsError> {
        let entry = self.fs().open_path(path).map_err(Self::map_error)?;
        self.fs().delete(&entry).map_err(Self::map_error)
    }

    fn readdir(&mut self, path: &str) -> Result<Vec<VNode>, FsError> {
        if path.trim_matches('/').is_empty() {
            return self.root_entries();
        }
        self.directory(path)?
            .entries()
            .map(|entry| {
                entry
                    .map(|entry| VNode {
                        size: entry.size(),
                        is_dir: entry.is_directory(),
                        name: entry.name,
                    })
                    .map_err(Self::map_error)
            })
            .collect()
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
        fn read_sectors(
            &mut self,
            lba: u32,
            count: u16,
            buf: &mut [u8],
        ) -> Result<(), &'static str> {
            let (start, len) = Self::range(lba, count)?;
            if buf.len() < len {
                return Err("test read buffer too small");
            }
            self.read_calls.fetch_add(1, Ordering::Relaxed);
            self.reads.fetch_add(count as usize, Ordering::Relaxed);
            buf[..len].copy_from_slice(&self.image.lock()[start..start + len]);
            Ok(())
        }

        fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
            let (start, len) = Self::range(lba, count)?;
            if buf.len() < len {
                return Err("test write buffer too small");
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

        fn range(lba: u32, count: u16) -> Result<(usize, usize), &'static str> {
            let start = lba as usize * SECTOR_SIZE;
            let len = count as usize * SECTOR_SIZE;
            if start.checked_add(len).is_none_or(|end| end > IMAGE_SIZE) {
                return Err("test LBA out of range");
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

        // A shift of 8 (256 sectors) overflowed the old u8 implementation and
        // triggered the magenta panic screen during `mount`.
        assert_eq!(image.lock()[109], 8);

        let mut fs = match crate::drivers::fat::mount_device(Box::new(block_device.clone())) {
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
        let mut fs = match crate::drivers::fat::mount_device(Box::new(block_device.clone())) {
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
        let mut fs = match crate::drivers::fat::mount_device(Box::new(block_device.clone())) {
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
}
