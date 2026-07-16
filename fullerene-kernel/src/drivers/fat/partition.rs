//! FAT-family partition discovery and partition-relative block access.

use alloc::boxed::Box;

use genome::fs::FsError;

use super::exfat::is_exfat;
use super::{BlockDevice, BlockError};
use crate::klog_fmt;

const MBR_SIGNATURE: u16 = 0xAA55;
const PARTITION_FAT32: u8 = 0x0B;
const PARTITION_FAT32_LBA: u8 = 0x0C;
const PARTITION_FAT16: u8 = 0x06;
const PARTITION_FAT16_LBA: u8 = 0x0E;
const PARTITION_EXFAT: u8 = 0x07;

pub fn find_fat_partition(device: &mut dyn BlockDevice) -> Result<u32, FsError> {
    let mut boot = [0u8; 512];
    device.read_sectors(0, 1, &mut boot)?;

    if is_exfat(&boot) {
        klog_fmt!("FAT: raw exFAT at LBA 0\n");
        return Ok(0);
    }
    let bytes_per_sector = u16::from_le_bytes([boot[11], boot[12]]);
    if matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096) {
        klog_fmt!("FAT: raw FAT32 at LBA 0 (bps={})\n", bytes_per_sector);
        return Ok(0);
    }

    let signature = u16::from_le_bytes([boot[0x1FE], boot[0x1FF]]);
    if signature != MBR_SIGNATURE {
        klog_fmt!("FAT: no MBR signature at LBA 0 (0x{:04X})\n", signature);
        return Ok(0);
    }

    let mut best_lba = None;
    let mut best_sectors = 0;
    for index in 0..4 {
        let offset = 0x1BE + index * 16;
        let partition_type = boot[offset + 4];
        let lba_start = u32::from_le_bytes([
            boot[offset + 8],
            boot[offset + 9],
            boot[offset + 10],
            boot[offset + 11],
        ]);
        let sector_count = u32::from_le_bytes([
            boot[offset + 12],
            boot[offset + 13],
            boot[offset + 14],
            boot[offset + 15],
        ]);
        let is_fat = matches!(
            partition_type,
            PARTITION_FAT32
                | PARTITION_FAT32_LBA
                | PARTITION_FAT16
                | PARTITION_FAT16_LBA
                | PARTITION_EXFAT
        );
        if is_fat && sector_count > best_sectors {
            best_lba = Some(lba_start);
            best_sectors = sector_count;
        }
    }

    if let Some(lba) = best_lba {
        klog_fmt!(
            "FAT: selected partition at LBA {} ({} sectors)\n",
            lba,
            best_sectors
        );
        return Ok(lba);
    }

    klog_fmt!("FAT: no FAT partition found in MBR\n");
    Err(FsError::FileNotFound)
}

pub struct PartitionBlockDevice {
    inner: Box<dyn BlockDevice>,
    offset: u32,
}

impl PartitionBlockDevice {
    pub fn new(inner: Box<dyn BlockDevice>, offset: u32) -> Self {
        Self { inner, offset }
    }

    fn absolute_lba(&self, lba: u32, count: u16) -> Result<u32, BlockError> {
        let absolute = lba
            .checked_add(self.offset)
            .ok_or(BlockError::LbaOverflow)?;
        let end = absolute as u64 + count as u64;
        if end > self.inner.total_sectors() || end > u32::MAX as u64 {
            return Err(BlockError::LbaOverflow);
        }
        Ok(absolute)
    }
}

impl BlockDevice for PartitionBlockDevice {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
        let absolute = self.absolute_lba(lba, count)?;
        self.inner.read_sectors(absolute, count, buf)
    }

    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), BlockError> {
        let absolute = self.absolute_lba(lba, count)?;
        self.inner.write_sectors(absolute, count, buf)
    }

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn total_sectors(&self) -> u64 {
        self.inner
            .total_sectors()
            .saturating_sub(self.offset as u64)
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;

    struct MemoryBlockDevice {
        data: Vec<u8>,
    }

    impl MemoryBlockDevice {
        fn with_boot_sector(boot: [u8; 512]) -> Self {
            Self { data: boot.into() }
        }
    }

    impl BlockDevice for MemoryBlockDevice {
        fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
            let start = lba as usize * 512;
            let len = count as usize * 512;
            let end = start.checked_add(len).ok_or(BlockError::LbaOverflow)?;
            if end > self.data.len() || buf.len() < len {
                return Err(BlockError::LbaOverflow);
            }
            buf[..len].copy_from_slice(&self.data[start..end]);
            Ok(())
        }

        fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), BlockError> {
            let start = lba as usize * 512;
            let len = count as usize * 512;
            let end = start.checked_add(len).ok_or(BlockError::LbaOverflow)?;
            if end > self.data.len() || buf.len() < len {
                return Err(BlockError::LbaOverflow);
            }
            self.data[start..end].copy_from_slice(&buf[..len]);
            Ok(())
        }

        fn sector_size(&self) -> u32 {
            512
        }

        fn total_sectors(&self) -> u64 {
            (self.data.len() / 512) as u64
        }
    }

    fn set_partition(boot: &mut [u8; 512], index: usize, kind: u8, lba: u32, sectors: u32) {
        let offset = 0x1BE + index * 16;
        boot[offset + 4] = kind;
        boot[offset + 8..offset + 12].copy_from_slice(&lba.to_le_bytes());
        boot[offset + 12..offset + 16].copy_from_slice(&sectors.to_le_bytes());
    }

    #[test]
    fn raw_fat_volume_uses_lba_zero() {
        let mut boot = [0; 512];
        boot[11..13].copy_from_slice(&512u16.to_le_bytes());
        let mut device = MemoryBlockDevice::with_boot_sector(boot);

        assert_eq!(find_fat_partition(&mut device), Ok(0));
    }

    #[test]
    fn mbr_selects_largest_supported_partition() {
        let mut boot = [0; 512];
        boot[0x1FE..].copy_from_slice(&MBR_SIGNATURE.to_le_bytes());
        set_partition(&mut boot, 0, PARTITION_FAT16, 32, 128);
        set_partition(&mut boot, 1, PARTITION_FAT32_LBA, 512, 4096);
        set_partition(&mut boot, 2, 0x83, 8192, 16_384);
        let mut device = MemoryBlockDevice::with_boot_sector(boot);

        assert_eq!(find_fat_partition(&mut device), Ok(512));
    }

    #[test]
    fn partition_device_rejects_reads_past_media_end() {
        let device = MemoryBlockDevice {
            data: vec![0; 4 * 512],
        };
        let mut partition = PartitionBlockDevice::new(Box::new(device), 2);
        let mut buf = [0; 512];

        assert_eq!(
            partition.read_sectors(2, 1, &mut buf),
            Err(BlockError::LbaOverflow)
        );
    }
}
