//! Adapts the kernel block-device contract to the `fatfs` I/O traits.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use fatfs::{IoBase, IoError as FatIoError, Read, Seek, SeekFrom, Write};

pub use genome::block::{BlockDevice, BlockError};

pub(super) fn read_boot_sector(
    device: &mut dyn BlockDevice,
    lba: u32,
) -> Result<[u8; 512], BlockError> {
    let sector_size = device.sector_size() as usize;
    if sector_size < 512 {
        return Err(BlockError::BufferTooSmall {
            required: 512,
            provided: sector_size,
        });
    }

    let mut sector = vec![0u8; sector_size];
    device.read_sectors(lba, 1, &mut sector)?;
    let mut boot = [0u8; 512];
    boot.copy_from_slice(&sector[..512]);
    Ok(boot)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FatBlockError {
    Device(BlockError),
    UnexpectedEof,
    WriteZero,
}

impl fmt::Display for FatBlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device(error) => write!(f, "device error: {}", error),
            Self::UnexpectedEof => write!(f, "unexpected eof"),
            Self::WriteZero => write!(f, "write zero"),
        }
    }
}

impl FatIoError for FatBlockError {
    fn is_interrupted(&self) -> bool {
        false
    }

    fn new_unexpected_eof_error() -> Self {
        Self::UnexpectedEof
    }

    fn new_write_zero_error() -> Self {
        Self::WriteZero
    }
}

pub struct FatDevice {
    device: Box<dyn BlockDevice>,
    pos: u64,
    bytes_per_sector: u32,
    total_bytes: u64,
    scratch: Vec<u8>,
}

impl FatDevice {
    pub fn new(device: Box<dyn BlockDevice>) -> Self {
        let bytes_per_sector = device.sector_size();
        let total_bytes = device
            .total_sectors()
            .saturating_mul(bytes_per_sector as u64);
        let scratch = vec![0u8; bytes_per_sector as usize];
        Self {
            device,
            pos: 0,
            bytes_per_sector,
            total_bytes,
            scratch,
        }
    }
}

impl IoBase for FatDevice {
    type Error = FatBlockError;
}

impl Read for FatDevice {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if self.pos >= self.total_bytes {
            return Ok(0);
        }

        let end = self
            .pos
            .saturating_add(buf.len() as u64)
            .min(self.total_bytes);
        let len = (end - self.pos) as usize;
        let mut written = 0usize;

        while written < len {
            let current_pos = self.pos + written as u64;
            let sector = (current_pos / self.bytes_per_sector as u64) as u32;
            let offset = (current_pos % self.bytes_per_sector as u64) as usize;
            self.device
                .read_sectors(sector, 1, &mut self.scratch)
                .map_err(FatBlockError::Device)?;
            let available = (self.bytes_per_sector as usize - offset).min(len - written);
            buf[written..written + available]
                .copy_from_slice(&self.scratch[offset..offset + available]);
            written += available;
        }

        self.pos += written as u64;
        Ok(written)
    }
}

impl Write for FatDevice {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() || self.pos >= self.total_bytes {
            return Ok(0);
        }

        let sector = (self.pos / self.bytes_per_sector as u64) as u32;
        let offset = (self.pos % self.bytes_per_sector as u64) as usize;
        let remaining = (self.total_bytes - self.pos) as usize;
        let write_len = buf
            .len()
            .min(remaining)
            .min((self.bytes_per_sector as usize).saturating_sub(offset));
        if write_len == 0 {
            return Ok(0);
        }

        if offset > 0 || write_len < self.bytes_per_sector as usize {
            self.device
                .read_sectors(sector, 1, &mut self.scratch)
                .map_err(FatBlockError::Device)?;
        }
        self.scratch[offset..offset + write_len].copy_from_slice(&buf[..write_len]);
        self.device
            .write_sectors(sector, 1, &self.scratch)
            .map_err(FatBlockError::Device)?;
        self.pos += write_len as u64;
        Ok(write_len)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Seek for FatDevice {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        let new_pos = match pos {
            SeekFrom::Start(offset) => offset,
            SeekFrom::End(offset) => self.total_bytes.saturating_add_signed(offset),
            SeekFrom::Current(offset) => self.pos.saturating_add_signed(offset),
        };
        self.pos = new_pos.min(self.total_bytes);
        Ok(self.pos)
    }
}
