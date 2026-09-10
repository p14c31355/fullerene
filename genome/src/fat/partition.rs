use alloc::boxed::Box;

use crate::block::{BlockDevice, BlockError};
use crate::fs::FsError;

use super::block_device::read_boot_sector;
use super::exfat::is_exfat;

const EFI_SYSTEM_PARTITION_GUID: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];
const MICROSOFT_BASIC_DATA_PARTITION_GUID: [u8; 16] = [
    0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7,
];

const MBR_SIGNATURE: u16 = 0xAA55;
const PARTITION_FAT32: u8 = 0x0B;
const PARTITION_FAT32_LBA: u8 = 0x0C;
const PARTITION_FAT16: u8 = 0x06;
const PARTITION_FAT16_LBA: u8 = 0x0E;
const PARTITION_EXFAT: u8 = 0x07;

pub struct PartitionInfo {
    pub start_lba: u64,
    pub total_sectors: u64,
}

pub fn find_fat_partition(device: &mut dyn BlockDevice) -> Result<PartitionInfo, FsError> {
    let boot = read_boot_sector(device, 0)?;

    if is_exfat(&boot) {
        log::info!("FAT: raw exFAT at LBA 0");
        return Ok(PartitionInfo {
            start_lba: 0,
            total_sectors: device.total_sectors(),
        });
    }
    let bytes_per_sector = u16::from_le_bytes([boot[11], boot[12]]);
    if matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096) {
        log::info!("FAT: raw FAT32 at LBA 0 (bps={})", bytes_per_sector);
        return Ok(PartitionInfo {
            start_lba: 0,
            total_sectors: device.total_sectors(),
        });
    }

    let signature = u16::from_le_bytes([boot[0x1FE], boot[0x1FF]]);
    if signature != MBR_SIGNATURE {
        if let Some(info) = find_gpt_fat_partition(device) {
            return Ok(info);
        }
        log::info!("FAT: no MBR signature at LBA 0 (0x{:04X})", signature);
        return Ok(PartitionInfo {
            start_lba: 0,
            total_sectors: device.total_sectors(),
        });
    }

    let mut best: Option<PartitionInfo> = None;
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
        if is_fat
            && (best
                .as_ref()
                .is_none_or(|b| sector_count > b.total_sectors as u32))
        {
            best = Some(PartitionInfo {
                start_lba: lba_start as u64,
                total_sectors: sector_count as u64,
            });
        }
    }

    if let Some(info) = best {
        log::info!(
            "FAT: selected partition at LBA {} ({} sectors)",
            info.start_lba,
            info.total_sectors,
        );
        return Ok(info);
    }

    if let Some(info) = find_gpt_fat_partition(device) {
        return Ok(info);
    }

    log::info!("FAT: no FAT partition found in MBR");
    Err(FsError::FileNotFound)
}

fn find_gpt_fat_partition(device: &mut dyn BlockDevice) -> Option<PartitionInfo> {
    let table = match crate::gpt::scan(device) {
        Ok(table) => table,
        Err(crate::gpt::GptError::InvalidSignature) => return None,
        Err(error) => {
            log::info!("FAT: GPT probe rejected: {:?}", error);
            return None;
        }
    };

    let mut best: Option<PartitionInfo> = None;
    for partition in table.partitions {
        let supported_type = partition.is_type(&EFI_SYSTEM_PARTITION_GUID)
            || partition.is_type(&MICROSOFT_BASIC_DATA_PARTITION_GUID);
        if !supported_type {
            continue;
        }
        if partition.is_type(&EFI_SYSTEM_PARTITION_GUID) {
            log::info!(
                "FAT: probing GPT EFI System Partition at LBA {}",
                partition.first_lba
            );
        }
        let boot = match read_boot_sector(device, partition.first_lba) {
            Ok(boot) => boot,
            Err(error) => {
                log::info!(
                    "FAT: GPT partition at LBA {} could not be read: {:?}",
                    partition.first_lba,
                    error
                );
                continue;
            }
        };
        let is_fat = is_exfat(&boot) || is_fat_boot_sector(&boot);
        if is_fat
            && best.as_ref().is_none_or(|current| {
                partition.last_lba - partition.first_lba + 1 > current.total_sectors
            })
        {
            best = Some(PartitionInfo {
                start_lba: partition.first_lba,
                total_sectors: partition.last_lba - partition.first_lba + 1,
            });
        }
    }

    if let Some(info) = best {
        log::info!(
            "FAT: selected GPT partition at LBA {} ({} sectors)",
            info.start_lba,
            info.total_sectors,
        );
        Some(info)
    } else {
        log::info!("FAT: GPT contains no FAT/exFAT volume");
        None
    }
}

fn is_fat_boot_sector(boot: &[u8]) -> bool {
    if boot.len() < 0x200 {
        return false;
    }
    if u16::from_le_bytes([boot[0x1fe], boot[0x1ff]]) != MBR_SIGNATURE {
        return false;
    }
    boot[54..62] == *b"FAT12   " || boot[54..62] == *b"FAT16   " || boot[82..90] == *b"FAT32   "
}

pub struct PartitionBlockDevice {
    inner: Box<dyn BlockDevice>,
    offset: u64,
    total_sectors: u64,
}

impl PartitionBlockDevice {
    pub fn new(inner: Box<dyn BlockDevice>, offset: u64, total_sectors: u64) -> Self {
        Self {
            inner,
            offset,
            total_sectors,
        }
    }

    fn absolute_lba(&self, lba: u64, count: u16) -> Result<u64, BlockError> {
        let absolute = lba
            .checked_add(self.offset)
            .ok_or(BlockError::LbaOverflow)?;
        let end = lba
            .checked_add(count as u64)
            .ok_or(BlockError::LbaOverflow)?;
        if end > self.total_sectors {
            return Err(BlockError::LbaOverflow);
        }
        let media_end = absolute
            .checked_add(count as u64)
            .ok_or(BlockError::LbaOverflow)?;
        if media_end > self.inner.total_sectors() {
            return Err(BlockError::LbaOverflow);
        }
        Ok(absolute)
    }
}

impl BlockDevice for PartitionBlockDevice {
    fn read_sectors(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
        let absolute = self.absolute_lba(lba, count)?;
        self.inner.read_sectors(absolute, count, buf)
    }

    fn write_sectors(&mut self, lba: u64, count: u16, buf: &[u8]) -> Result<(), BlockError> {
        let absolute = self.absolute_lba(lba, count)?;
        self.inner.write_sectors(absolute, count, buf)
    }

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn total_sectors(&self) -> u64 {
        self.total_sectors
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

    struct FourKnBlockDevice {
        sector: [u8; 4096],
    }

    impl BlockDevice for FourKnBlockDevice {
        fn read_sectors(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
            if lba != 0 || count != 1 {
                return Err(BlockError::LbaOverflow);
            }
            if buf.len() < self.sector.len() {
                return Err(BlockError::BufferTooSmall {
                    required: self.sector.len(),
                    provided: buf.len(),
                });
            }
            buf[..self.sector.len()].copy_from_slice(&self.sector);
            Ok(())
        }

        fn write_sectors(&mut self, _lba: u64, _count: u16, _buf: &[u8]) -> Result<(), BlockError> {
            Err(BlockError::Device)
        }

        fn sector_size(&self) -> u32 {
            4096
        }

        fn total_sectors(&self) -> u64 {
            1
        }
    }

    impl MemoryBlockDevice {
        fn with_boot_sector(boot: [u8; 512]) -> Self {
            Self { data: boot.into() }
        }
    }

    impl BlockDevice for MemoryBlockDevice {
        fn read_sectors(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
            let start = lba as usize * 512;
            let len = count as usize * 512;
            let end = start.checked_add(len).ok_or(BlockError::LbaOverflow)?;
            if end > self.data.len() || buf.len() < len {
                return Err(BlockError::LbaOverflow);
            }
            buf[..len].copy_from_slice(&self.data[start..end]);
            Ok(())
        }

        fn write_sectors(&mut self, lba: u64, count: u16, buf: &[u8]) -> Result<(), BlockError> {
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

        assert_eq!(find_fat_partition(&mut device).map(|i| i.start_lba), Ok(0));
    }

    #[test]
    fn raw_fat_volume_supports_four_kn_media() {
        let mut sector = [0; 4096];
        sector[11..13].copy_from_slice(&4096u16.to_le_bytes());
        let mut device = FourKnBlockDevice { sector };

        assert_eq!(find_fat_partition(&mut device).map(|i| i.start_lba), Ok(0));
    }

    #[test]
    fn mbr_selects_largest_supported_partition() {
        let mut boot = [0; 512];
        boot[0x1FE..].copy_from_slice(&MBR_SIGNATURE.to_le_bytes());
        set_partition(&mut boot, 0, PARTITION_FAT16, 32, 128);
        set_partition(&mut boot, 1, PARTITION_FAT32_LBA, 512, 4096);
        set_partition(&mut boot, 2, 0x83, 8192, 16_384);
        let mut device = MemoryBlockDevice::with_boot_sector(boot);

        let info = find_fat_partition(&mut device).unwrap();
        assert_eq!(info.start_lba, 512);
        assert_eq!(info.total_sectors, 4096);
    }

    #[test]
    fn gpt_selects_largest_fat_partition() {
        let mut disk = MemoryBlockDevice {
            data: vec![0; 1_000 * 512],
        };
        let header = &mut disk.data[512..1024];
        header[..8].copy_from_slice(b"EFI PART");
        header[12..16].copy_from_slice(&92u32.to_le_bytes());
        header[24..32].copy_from_slice(&1u64.to_le_bytes());
        header[32..40].copy_from_slice(&999u64.to_le_bytes());
        header[40..48].copy_from_slice(&34u64.to_le_bytes());
        header[48..56].copy_from_slice(&900u64.to_le_bytes());
        header[72..80].copy_from_slice(&2u64.to_le_bytes());
        header[80..84].copy_from_slice(&4u32.to_le_bytes());
        header[84..88].copy_from_slice(&128u32.to_le_bytes());

        let small = &mut disk.data[2 * 512..3 * 512];
        small[..16].copy_from_slice(&EFI_SYSTEM_PARTITION_GUID);
        small[32..40].copy_from_slice(&100u64.to_le_bytes());
        small[40..48].copy_from_slice(&199u64.to_le_bytes());
        let large = &mut disk.data[2 * 512 + 128..3 * 512];
        large[..16].copy_from_slice(&MICROSOFT_BASIC_DATA_PARTITION_GUID);
        large[32..40].copy_from_slice(&200u64.to_le_bytes());
        large[40..48].copy_from_slice(&499u64.to_le_bytes());

        disk.data[100 * 512 + 54..100 * 512 + 62].copy_from_slice(b"FAT16   ");
        disk.data[200 * 512 + 82..200 * 512 + 90].copy_from_slice(b"FAT32   ");
        disk.data[100 * 512 + 0x1fe..100 * 512 + 0x200]
            .copy_from_slice(&MBR_SIGNATURE.to_le_bytes());
        disk.data[200 * 512 + 0x1fe..200 * 512 + 0x200]
            .copy_from_slice(&MBR_SIGNATURE.to_le_bytes());

        let info = find_fat_partition(&mut disk).unwrap();
        assert_eq!(info.start_lba, 200);
        assert_eq!(info.total_sectors, 300);
    }

    #[test]
    fn partition_device_rejects_reads_past_partition_end() {
        let device = MemoryBlockDevice {
            data: vec![0; 8 * 512],
        };
        let mut partition = PartitionBlockDevice::new(Box::new(device), 2, 2);
        let mut buf = [0; 512];

        assert!(partition.read_sectors(0, 1, &mut buf).is_ok());
        assert!(partition.read_sectors(1, 1, &mut buf).is_ok());
        assert_eq!(
            partition.read_sectors(2, 1, &mut buf),
            Err(BlockError::LbaOverflow)
        );
        assert_eq!(
            partition.read_sectors(5, 1, &mut buf),
            Err(BlockError::LbaOverflow)
        );
    }
}
