//! Adapter from Nitrogen's SDMMC mechanism to Genome's block contract.

use crate::block::{BlockDevice, BlockError};
use alloc::boxed::Box;
use nitrogen::arch::xtensa::esp32::sdmmc::{SdMmcError, SdMmcHost};

pub struct GenomeSdMmc {
    host: SdMmcHost,
}

impl GenomeSdMmc {
    pub fn probe() -> Result<Self, SdMmcError> {
        let mut host = SdMmcHost::new();
        host.detect()?;
        Ok(Self { host })
    }

    pub fn into_block_device(self) -> Box<dyn BlockDevice> {
        Box::new(self)
    }
}

impl BlockDevice for GenomeSdMmc {
    fn read_sectors(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
        self.host
            .read_sectors_raw(lba, count, buf)
            .map_err(|error| match error {
                SdMmcError::BufferTooSmall { required, provided } => {
                    BlockError::BufferTooSmall { required, provided }
                }
                _ => BlockError::Device,
            })
    }

    fn write_sectors(&mut self, lba: u64, count: u16, buf: &[u8]) -> Result<(), BlockError> {
        self.host
            .write_sectors_raw(lba, count, buf)
            .map_err(|error| match error {
                SdMmcError::BufferTooSmall { required, provided } => {
                    BlockError::BufferTooSmall { required, provided }
                }
                _ => BlockError::Device,
            })
    }

    fn sector_size(&self) -> u32 {
        512
    }

    fn total_sectors(&self) -> u64 {
        self.host.sector_count()
    }
}
