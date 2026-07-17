//! FAT12/16/32 VFS implementation backed by `fatfs`.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use fatfs::{self, DefaultTimeProvider, LossyOemCpConverter, Read, Seek, SeekFrom, Write};
use genome::fs::FsError;

use super::{BlockDevice, FatBlockError, FatDevice};
use crate::contexts::vfs::{FileDescriptor, FileSystem, FileSystemCapabilities, InodeType, VNode};
use crate::klog_fmt;

type FatType = fatfs::FileSystem<FatDevice>;
type FatDir<'a> = fatfs::Dir<'a, FatDevice, DefaultTimeProvider, LossyOemCpConverter>;
type FatFile<'a> = fatfs::File<'a, FatDevice, DefaultTimeProvider, LossyOemCpConverter>;
type FatError = fatfs::Error<FatBlockError>;

pub struct FatFileSystem {
    inner: FatType,
    next_fd: u32,
    handles: Vec<(u32, String, u64)>,
}

impl FatFileSystem {
    pub fn new(device: Box<dyn BlockDevice>) -> Result<Self, FsError> {
        let fat_device = FatDevice::new(device);
        let options = fatfs::FsOptions::new();
        let inner = FatType::new(fat_device, options).map_err(|error| {
            klog_fmt!("FAT: fatfs init error: {:?}\n", error);
            FsError::InvalidInput
        })?;
        Ok(Self {
            inner,
            next_fd: 1,
            handles: Vec::new(),
        })
    }

    fn map_err<T>(result: Result<T, FatError>) -> Result<T, FsError> {
        result.map_err(|error| match error {
            fatfs::Error::Io(FatBlockError::Device(error)) => error.into(),
            fatfs::Error::NotFound => FsError::FileNotFound,
            fatfs::Error::AlreadyExists => FsError::FileExists,
            fatfs::Error::NotEnoughSpace => FsError::DiskFull,
            fatfs::Error::CorruptedFileSystem => FsError::InvalidInput,
            _ => FsError::InvalidInput,
        })
    }

    fn open_dir<'a>(&'a mut self, path: &str) -> Result<FatDir<'a>, FsError> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            Ok(self.inner.root_dir())
        } else {
            Self::map_err(self.inner.root_dir().open_dir(path))
        }
    }

    fn open_file<'a>(&'a mut self, path: &str) -> Result<FatFile<'a>, FsError> {
        let path = path.trim_matches('/');
        Self::map_err(self.inner.root_dir().open_file(path))
    }

    fn create_file<'a>(&'a mut self, path: &str) -> Result<FatFile<'a>, FsError> {
        let path = path.trim_matches('/');
        let (parent, name) = match path.rfind('/') {
            Some(position) => (&path[..position], &path[position + 1..]),
            None => ("", path),
        };
        let directory = self.open_dir(parent)?;
        Self::map_err(directory.create_file(name))
    }
}

impl FileSystem for FatFileSystem {
    fn capabilities(&self) -> FileSystemCapabilities {
        FileSystemCapabilities::new(false, true, true, false, false)
    }

    fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor> {
        let _ = self.open_file(path).ok()?;
        let fd = self.next_fd;
        self.next_fd += 1;
        self.handles.push((fd, String::from(path), 0));
        Some(FileDescriptor {
            fd,
            ino: 0,
            offset: 0,
            flags,
        })
    }

    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        let (path, offset) = {
            let handle = self
                .handles
                .iter_mut()
                .find(|handle| handle.0 == fd)
                .ok_or(FsError::InvalidFileDescriptor)?;
            (handle.1.clone(), handle.2)
        };
        let bytes_read = {
            let mut file = self.open_file(&path)?;
            Self::map_err(file.seek(SeekFrom::Start(offset)))?;
            Self::map_err(file.read(buf))?
        };
        if let Some(handle) = self.handles.iter_mut().find(|handle| handle.0 == fd) {
            handle.2 += bytes_read as u64;
        }
        Ok(bytes_read)
    }

    fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, FsError> {
        let (path, offset) = {
            let handle = self
                .handles
                .iter_mut()
                .find(|handle| handle.0 == fd)
                .ok_or(FsError::InvalidFileDescriptor)?;
            (handle.1.clone(), handle.2)
        };
        let bytes_written = {
            let mut file = self.open_file(&path)?;
            Self::map_err(file.seek(SeekFrom::Start(offset)))?;
            Self::map_err(file.write(data))?
        };
        if let Some(handle) = self.handles.iter_mut().find(|handle| handle.0 == fd) {
            handle.2 += bytes_written as u64;
        }
        Ok(bytes_written)
    }

    fn close(&mut self, fd: u32) -> Result<(), FsError> {
        let position = self
            .handles
            .iter()
            .position(|handle| handle.0 == fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
        self.handles.remove(position);
        Ok(())
    }

    fn seek(&mut self, fd: u32, new_pos: u64) -> Result<(), FsError> {
        let handle = self
            .handles
            .iter_mut()
            .find(|handle| handle.0 == fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
        handle.2 = new_pos;
        Ok(())
    }

    fn create(&mut self, path: &str, _kind: InodeType) -> Option<u64> {
        let _file = self.create_file(path).ok()?;
        Some(0)
    }

    fn mkdir(&mut self, path: &str) -> Result<(), FsError> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return Err(FsError::InvalidInput);
        }
        let (parent, name) = match path.rfind('/') {
            Some(position) => (&path[..position], &path[position + 1..]),
            None => ("", path),
        };
        let directory = self.open_dir(parent)?;
        directory
            .create_dir(name)
            .map(|_| ())
            .map_err(|error| match error {
                fatfs::Error::AlreadyExists => FsError::FileExists,
                _ => FsError::InvalidInput,
            })
    }

    fn unlink(&mut self, path: &str) -> Result<(), FsError> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return Err(FsError::InvalidInput);
        }
        let (parent, name) = match path.rfind('/') {
            Some(position) => (&path[..position], &path[position + 1..]),
            None => ("", path),
        };
        let directory = self.open_dir(parent)?;
        Self::map_err(directory.remove(name))
    }

    fn readdir(&mut self, path: &str) -> Result<Vec<VNode>, FsError> {
        let directory = self.open_dir(path)?;
        let mut result = Vec::new();
        for entry in directory.iter() {
            let entry = Self::map_err(entry)?;
            result.push(VNode {
                name: entry.file_name(),
                size: entry.len(),
                is_dir: entry.is_dir(),
            });
        }
        Ok(result)
    }

    fn exists(&mut self, path: &str) -> bool {
        self.open_file(path).is_ok() || self.open_dir(path).is_ok()
    }
}
