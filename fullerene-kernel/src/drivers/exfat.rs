//! exFAT filesystem adapter for the kernel VFS.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use aligned::{A4, Aligned};
use exfat_slim::blocking::{
    BlockDevice as ExFatBlockDevice, error::ExFatError, file::OpenOptions,
    file_system::FileSystem as ExFat,
};
use genome::fs::FsError;

use crate::contexts::vfs::{FileDescriptor, FileSystem, InodeType, VNode};
use crate::drivers::fat::BlockDevice;
use crate::klog_fmt;

const SECTOR_SIZE: usize = 512;
const CACHE_SLOTS: usize = 8;
type ExFatErrorKind = ExFatError<&'static str>;

struct ExFatDevice {
    inner: Box<dyn BlockDevice>,
}

impl ExFatBlockDevice<SECTOR_SIZE> for ExFatDevice {
    type Error = &'static str;
    type Align = A4;

    fn read(
        &mut self,
        lba: u32,
        blocks: &mut [Aligned<Self::Align, [u8; SECTOR_SIZE]>],
    ) -> Result<(), Self::Error> {
        blocks
            .iter_mut()
            .enumerate()
            .try_for_each(|(index, block)| {
                self.inner.read_sectors(
                    lba.checked_add(index as u32).ok_or("exFAT LBA overflow")?,
                    1,
                    &mut block[..],
                )
            })
    }

    fn write(
        &mut self,
        lba: u32,
        blocks: &[Aligned<Self::Align, [u8; SECTOR_SIZE]>],
    ) -> Result<(), Self::Error> {
        blocks.iter().enumerate().try_for_each(|(index, block)| {
            self.inner.write_sectors(
                lba.checked_add(index as u32).ok_or("exFAT LBA overflow")?,
                1,
                &block[..],
            )
        })
    }

    fn size(&mut self) -> Result<u64, Self::Error> {
        self.inner
            .total_sectors()
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or("exFAT device size overflow")
    }
}

type ExFatInner = ExFat<ExFatDevice, SECTOR_SIZE, CACHE_SLOTS>;

pub struct ExFatFileSystem {
    inner: ExFatInner,
    next_fd: u32,
    handles: Vec<(u32, String, u64)>,
}

impl ExFatFileSystem {
    pub fn new(
        device: Box<dyn BlockDevice>,
    ) -> Result<Self, (FsError, Option<Box<dyn BlockDevice>>)> {
        if device.sector_size() as usize != SECTOR_SIZE {
            return Err((FsError::NotSupported, Some(device)));
        }
        let mut inner = ExFat::new(ExFatDevice { inner: device });
        if let Err(error) = inner.mount() {
            klog_fmt!("exFAT: mount error: {:?}\n", error);
            return Err((Self::map_error(error), Some(inner.unmount().inner)));
        }
        klog_fmt!("exFAT: volume mounted\n");
        Ok(Self {
            inner,
            next_fd: 1,
            handles: Vec::new(),
        })
    }

    fn map_error(error: ExFatErrorKind) -> FsError {
        match error {
            ExFatError::FileNotFound | ExFatError::DirectoryNotFound => FsError::FileNotFound,
            ExFatError::AlreadyExists => FsError::FileExists,
            ExFatError::DiskFull => FsError::DiskFull,
            ExFatError::DirectoryNotEmpty => FsError::DirectoryNotEmpty,
            ExFatError::SeekOutOfRange => FsError::InvalidSeek,
            ExFatError::WriteNotEnabled | ExFatError::ReadNotEnabled => FsError::PermissionDenied,
            ExFatError::InvalidFileName { .. } => FsError::InvalidPath,
            _ => FsError::InvalidInput,
        }
    }

    fn handle(&mut self, fd: u32) -> Result<(String, u64), FsError> {
        self.handles
            .iter()
            .find(|handle| handle.0 == fd)
            .map(|handle| (handle.1.clone(), handle.2))
            .ok_or(FsError::InvalidFileDescriptor)
    }
}

impl FileSystem for ExFatFileSystem {
    fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor> {
        self.inner.open(path, OpenOptions::new().read(true)).ok()?;
        let fd = self.next_fd;
        self.next_fd = self.next_fd.wrapping_add(1);
        self.handles.push((fd, String::from(path), 0));
        Some(FileDescriptor {
            fd,
            ino: 0,
            offset: 0,
            flags,
        })
    }

    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        let (path, offset) = self.handle(fd)?;
        let mut file = self
            .inner
            .open(&path, OpenOptions::new().read(true))
            .map_err(Self::map_error)?;
        if offset != 0 {
            file.seek(&mut self.inner, offset)
                .map_err(Self::map_error)?;
        }
        let read = file
            .read(&mut self.inner, buf)
            .map_err(Self::map_error)?
            .unwrap_or(0);
        if let Some(handle) = self.handles.iter_mut().find(|handle| handle.0 == fd) {
            handle.2 += read as u64;
        }
        Ok(read)
    }

    fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, FsError> {
        let (path, offset) = self.handle(fd)?;
        let mut file = self
            .inner
            .open(&path, OpenOptions::new().write(true))
            .map_err(Self::map_error)?;
        if offset != 0 {
            file.seek(&mut self.inner, offset)
                .map_err(Self::map_error)?;
        }
        file.write(&mut self.inner, data).map_err(Self::map_error)?;
        file.close(&mut self.inner).map_err(Self::map_error)?;
        if let Some(handle) = self.handles.iter_mut().find(|handle| handle.0 == fd) {
            handle.2 += data.len() as u64;
        }
        Ok(data.len())
    }

    fn close(&mut self, fd: u32) -> Result<(), FsError> {
        let index = self
            .handles
            .iter()
            .position(|handle| handle.0 == fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
        self.handles.remove(index);
        Ok(())
    }

    fn seek(&mut self, fd: u32, new_pos: usize) -> Result<(), FsError> {
        let handle = self
            .handles
            .iter_mut()
            .find(|handle| handle.0 == fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
        handle.2 = new_pos as u64;
        Ok(())
    }

    fn create(&mut self, path: &str, kind: InodeType) -> Option<u64> {
        match kind {
            InodeType::Directory => self.inner.create_directory(path).ok()?,
            InodeType::File => {
                let file = self
                    .inner
                    .open(path, OpenOptions::new().create_new(true).write(true))
                    .ok()?;
                file.close(&mut self.inner).ok()?;
            }
            InodeType::Symlink => return None,
        }
        Some(0)
    }

    fn mkdir(&mut self, path: &str) -> Result<(), FsError> {
        self.inner.create_directory(path).map_err(Self::map_error)
    }

    fn unlink(&mut self, path: &str) -> Result<(), FsError> {
        match self.inner.remove_file(path) {
            Ok(()) => Ok(()),
            Err(ExFatError::FileNotFound) => self.inner.remove_dir(path).map_err(Self::map_error),
            Err(error) => Err(Self::map_error(error)),
        }
    }

    fn readdir(&mut self, path: &str) -> Result<Vec<VNode>, FsError> {
        let mut directory = self.inner.read_dir(path).map_err(Self::map_error)?;
        let mut entries = Vec::new();
        while let Some(entry) = directory
            .next_entry(&mut self.inner)
            .map_err(Self::map_error)?
        {
            let metadata = entry.metadata();
            entries.push(VNode {
                name: entry.file_name(),
                size: metadata.len(),
                is_dir: metadata.is_dir(),
            });
        }
        Ok(entries)
    }

    fn exists(&mut self, path: &str) -> bool {
        self.inner.exists(path).unwrap_or(false)
    }
}
