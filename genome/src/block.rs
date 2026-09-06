use alloc::boxed::Box;
use alloc::vec;
use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    Device,
    Busy,
    BufferTooSmall { required: usize, provided: usize },
    LbaOverflow,
    SectorNotFound,
}

impl fmt::Display for BlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlockError::Device => write!(f, "block device error"),
            BlockError::Busy => write!(f, "block device busy"),
            BlockError::BufferTooSmall { required, provided } => {
                write!(f, "buffer too small: need {} got {}", required, provided)
            }
            BlockError::LbaOverflow => write!(f, "LBA overflow"),
            BlockError::SectorNotFound => write!(f, "sector not found"),
        }
    }
}

pub trait BlockDevice: Send {
    fn read_sectors(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError>;
    fn write_sectors(&mut self, lba: u64, count: u16, buf: &[u8]) -> Result<(), BlockError>;
    fn sector_size(&self) -> u32;
    fn total_sectors(&self) -> u64;
}

/// A read-only 512-byte sector view over a device with a larger logical
/// sector size. Android's logical-partition metadata is always expressed in
/// 512-byte sectors, while UFS commonly exposes 4096-byte sectors.
pub struct Sector512Device {
    inner: Box<dyn BlockDevice>,
    base_sector: u64,
    total_sectors_512: u64,
}

impl Sector512Device {
    pub fn new(inner: Box<dyn BlockDevice>, base_sector: u64, total_sectors_512: u64) -> Self {
        Self {
            inner,
            base_sector,
            total_sectors_512,
        }
    }

    fn read_512(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
        let byte_count = (count as usize)
            .checked_mul(512)
            .ok_or(BlockError::LbaOverflow)?;
        if buf.len() < byte_count {
            return Err(BlockError::BufferTooSmall {
                required: byte_count,
                provided: buf.len(),
            });
        }
        let end = lba
            .checked_add(count as u64)
            .ok_or(BlockError::LbaOverflow)?;
        if end > self.total_sectors_512 {
            return Err(BlockError::LbaOverflow);
        }
        if count == 0 {
            return Ok(());
        }

        let inner_sector_size = self.inner.sector_size() as u64;
        if inner_sector_size < 512 || !inner_sector_size.is_multiple_of(512) {
            return Err(BlockError::Device);
        }
        let start_byte = lba
            .checked_add(self.base_sector)
            .and_then(|sector| sector.checked_mul(512))
            .ok_or(BlockError::LbaOverflow)?;
        let end_byte = start_byte
            .checked_add(byte_count as u64)
            .ok_or(BlockError::LbaOverflow)?;
        let media_end_byte = self
            .inner
            .total_sectors()
            .checked_mul(inner_sector_size)
            .ok_or(BlockError::LbaOverflow)?;
        if end_byte > media_end_byte {
            return Err(BlockError::LbaOverflow);
        }

        let mut scratch = vec![0u8; inner_sector_size as usize];
        let first_inner_lba = start_byte / inner_sector_size;
        let last_inner_lba = (end_byte - 1) / inner_sector_size;
        let mut copied = 0usize;
        for inner_lba in first_inner_lba..=last_inner_lba {
            self.inner.read_sectors(inner_lba, 1, &mut scratch)?;
            let inner_start = inner_lba * inner_sector_size;
            let copy_start = start_byte.max(inner_start);
            let copy_end = end_byte.min(inner_start + inner_sector_size);
            let source_offset = (copy_start - inner_start) as usize;
            let copy_len = (copy_end - copy_start) as usize;
            buf[copied..copied + copy_len]
                .copy_from_slice(&scratch[source_offset..source_offset + copy_len]);
            copied += copy_len;
        }
        Ok(())
    }
}

impl BlockDevice for Sector512Device {
    fn read_sectors(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
        self.read_512(lba, count, buf)
    }

    fn write_sectors(&mut self, _lba: u64, _count: u16, _buf: &[u8]) -> Result<(), BlockError> {
        Err(BlockError::Device)
    }

    fn sector_size(&self) -> u32 {
        512
    }

    fn total_sectors(&self) -> u64 {
        self.total_sectors_512
    }
}

impl BlockDevice for alloc::boxed::Box<dyn BlockDevice> {
    fn read_sectors(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
        (**self).read_sectors(lba, count, buf)
    }
    fn write_sectors(&mut self, lba: u64, count: u16, buf: &[u8]) -> Result<(), BlockError> {
        (**self).write_sectors(lba, count, buf)
    }
    fn sector_size(&self) -> u32 {
        (**self).sector_size()
    }
    fn total_sectors(&self) -> u64 {
        (**self).total_sectors()
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    struct FourKnDevice {
        data: Vec<u8>,
    }

    impl BlockDevice for FourKnDevice {
        fn read_sectors(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
            let start = lba as usize * 4096;
            let len = count as usize * 4096;
            let end = start.checked_add(len).ok_or(BlockError::LbaOverflow)?;
            if end > self.data.len() || buf.len() < len {
                return Err(BlockError::LbaOverflow);
            }
            buf[..len].copy_from_slice(&self.data[start..end]);
            Ok(())
        }

        fn write_sectors(&mut self, _lba: u64, _count: u16, _buf: &[u8]) -> Result<(), BlockError> {
            Err(BlockError::Device)
        }

        fn sector_size(&self) -> u32 {
            4096
        }

        fn total_sectors(&self) -> u64 {
            (self.data.len() / 4096) as u64
        }
    }

    #[test]
    fn sector_512_view_translates_four_kib_media() {
        let mut data = vec![0u8; 2 * 4096];
        data[4096..8192].fill(0x5A);
        let mut view = Sector512Device::new(Box::new(FourKnDevice { data }), 8, 16);
        let mut sectors = [0u8; 1024];
        view.read_sectors(6, 2, &mut sectors).unwrap();
        assert_eq!(sectors, [0x5A; 1024]);
        assert_eq!(view.write_sectors(0, 1, &[0; 512]), Err(BlockError::Device));
    }
}
