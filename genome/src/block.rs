use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    Device,
    BufferTooSmall { required: usize, provided: usize },
    LbaOverflow,
    SectorNotFound,
}

impl fmt::Display for BlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlockError::Device => write!(f, "block device error"),
            BlockError::BufferTooSmall { required, provided } => {
                write!(f, "buffer too small: need {} got {}", required, provided)
            }
            BlockError::LbaOverflow => write!(f, "LBA overflow"),
            BlockError::SectorNotFound => write!(f, "sector not found"),
        }
    }
}

pub trait BlockDevice: Send {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), BlockError>;
    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), BlockError>;
    fn sector_size(&self) -> u32;
    fn total_sectors(&self) -> u64;
}

impl BlockDevice for alloc::boxed::Box<dyn BlockDevice> {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
        (**self).read_sectors(lba, count, buf)
    }
    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), BlockError> {
        (**self).write_sectors(lba, count, buf)
    }
    fn sector_size(&self) -> u32 {
        (**self).sector_size()
    }
    fn total_sectors(&self) -> u64 {
        (**self).total_sectors()
    }
}
