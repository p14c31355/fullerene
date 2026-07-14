use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    Device(&'static str),
    BufferTooSmall { required: usize, provided: usize },
    LbaOverflow,
    SectorNotFound,
}

impl fmt::Display for BlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlockError::Device(msg) => write!(f, "device error: {}", msg),
            BlockError::BufferTooSmall { required, provided } => {
                write!(f, "buffer too small: need {} got {}", required, provided)
            }
            BlockError::LbaOverflow => write!(f, "LBA overflow"),
            BlockError::SectorNotFound => write!(f, "sector not found"),
        }
    }
}

impl From<&'static str> for BlockError {
    fn from(e: &'static str) -> Self {
        BlockError::Device(e)
    }
}

pub trait BlockDevice: Send {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str>;
    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str>;
    fn sector_size(&self) -> u32;
    fn total_sectors(&self) -> u64;
}

impl BlockDevice for alloc::boxed::Box<dyn BlockDevice> {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str> {
        (**self).read_sectors(lba, count, buf)
    }
    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
        (**self).write_sectors(lba, count, buf)
    }
    fn sector_size(&self) -> u32 {
        (**self).sector_size()
    }
    fn total_sectors(&self) -> u64 {
        (**self).total_sectors()
    }
}
