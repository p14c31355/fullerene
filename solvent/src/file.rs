//! Runtime file handles supplied by the kernel capability bridge.

use genome::FsError;
use genome::io::{FileReader, Read, Seek, SeekFrom};

use crate::RUNTIME_CONTEXT;
use crate::callbacks::{
    VfsCloseCallback, VfsHandle, VfsStreamReadCallback, VfsStreamSeekCallback,
    VfsStreamSizeCallback,
};

pub struct RuntimeFile {
    handle: VfsHandle,
    read: VfsStreamReadCallback,
    seek: VfsStreamSeekCallback,
    size: VfsStreamSizeCallback,
    close: VfsCloseCallback,
}

impl RuntimeFile {
    pub fn open(path: &str) -> Result<Self, FsError> {
        let callbacks = RUNTIME_CONTEXT.callback_snapshot();
        let open = callbacks.vfs_open.ok_or(FsError::NotSupported)?;
        let read = callbacks.vfs_stream_read.ok_or(FsError::NotSupported)?;
        let seek = callbacks.vfs_stream_seek.ok_or(FsError::NotSupported)?;
        let size = callbacks.vfs_stream_size.ok_or(FsError::NotSupported)?;
        let close = callbacks.vfs_close.ok_or(FsError::NotSupported)?;
        let handle = open(path)?;
        Ok(Self {
            handle,
            read,
            seek,
            size,
            close,
        })
    }

    pub fn handle(&self) -> VfsHandle {
        self.handle
    }
}

impl Read for RuntimeFile {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError> {
        (self.read)(self.handle, buf)
    }
}

impl Seek for RuntimeFile {
    fn seek(&mut self, position: SeekFrom) -> Result<u64, FsError> {
        (self.seek)(self.handle, position)
    }
}

impl FileReader for RuntimeFile {
    fn len(&mut self) -> Result<u64, FsError> {
        (self.size)(self.handle)
    }
}

impl Drop for RuntimeFile {
    fn drop(&mut self) {
        let _ = (self.close)(self.handle);
    }
}
