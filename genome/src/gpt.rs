//! Read-only GPT discovery for block devices.
//!
//! The parser deliberately does not validate or modify on-disk CRCs.  It
//! validates the structural bounds needed before a caller reads partition
//! entries, which is sufficient for safe filesystem probing while keeping the
//! block-device contract small.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::block::{BlockDevice, BlockError};

const GPT_SIGNATURE: &[u8; 8] = b"EFI PART";
const GPT_HEADER_MIN_SIZE: u32 = 92;
const GPT_ENTRY_MIN_SIZE: u32 = 128;
const GPT_ENTRY_MAX_SIZE: u32 = 4096;
const GPT_ENTRY_ARRAY_MAX_BYTES: usize = 1024 * 1024;
const GPT_NAME_CODE_UNITS: usize = 36;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GptError {
    Device(BlockError),
    InvalidSectorSize,
    InvalidSignature,
    InvalidHeaderSize,
    InvalidHeaderLba,
    InvalidUsableRange,
    InvalidEntrySize,
    InvalidEntryArray,
    InvalidPartitionRange,
}

impl From<BlockError> for GptError {
    fn from(error: BlockError) -> Self {
        Self::Device(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GptHeader {
    pub current_lba: u64,
    pub backup_lba: u64,
    pub first_usable_lba: u64,
    pub last_usable_lba: u64,
    pub disk_guid: [u8; 16],
    pub partition_entries_lba: u64,
    pub partition_count: u32,
    pub partition_entry_size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GptPartition {
    pub type_guid: [u8; 16],
    pub unique_guid: [u8; 16],
    pub first_lba: u64,
    pub last_lba: u64,
    pub attributes: u64,
    name: [u16; GPT_NAME_CODE_UNITS],
}

impl GptPartition {
    pub fn is_type(&self, type_guid: &[u8; 16]) -> bool {
        &self.type_guid == type_guid
    }

    pub fn name(&self) -> String {
        let end = self
            .name
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(self.name.len());
        String::from_utf16_lossy(&self.name[..end])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GptTable {
    pub header: GptHeader,
    pub partitions: Vec<GptPartition>,
}

fn le_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn le_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

pub fn parse_header(
    sector: &[u8],
    sector_size: u32,
    total_sectors: u64,
) -> Result<GptHeader, GptError> {
    if sector_size < 512 || sector.len() < 512 {
        return Err(GptError::InvalidSectorSize);
    }
    if &sector[..8] != GPT_SIGNATURE {
        return Err(GptError::InvalidSignature);
    }

    let header_size = le_u32(sector, 12);
    if !(GPT_HEADER_MIN_SIZE..=sector_size).contains(&header_size) {
        return Err(GptError::InvalidHeaderSize);
    }

    let current_lba = le_u64(sector, 24);
    let backup_lba = le_u64(sector, 32);
    let first_usable_lba = le_u64(sector, 40);
    let last_usable_lba = le_u64(sector, 48);
    let mut disk_guid = [0u8; 16];
    disk_guid.copy_from_slice(&sector[56..72]);
    let partition_entries_lba = le_u64(sector, 72);
    let partition_count = le_u32(sector, 80);
    let partition_entry_size = le_u32(sector, 84);

    if total_sectors == 0
        || current_lba != 1
        || backup_lba >= total_sectors
        || first_usable_lba > last_usable_lba
        || last_usable_lba >= total_sectors
    {
        return Err(GptError::InvalidHeaderLba);
    }
    if partition_count == 0
        || !(GPT_ENTRY_MIN_SIZE..=GPT_ENTRY_MAX_SIZE).contains(&partition_entry_size)
        || !partition_entry_size.is_multiple_of(8)
    {
        return Err(GptError::InvalidEntrySize);
    }

    let array_bytes = (partition_count as usize)
        .checked_mul(partition_entry_size as usize)
        .ok_or(GptError::InvalidEntryArray)?;
    if array_bytes == 0 || array_bytes > GPT_ENTRY_ARRAY_MAX_BYTES {
        return Err(GptError::InvalidEntryArray);
    }
    let array_sectors = array_bytes
        .checked_add(sector_size as usize - 1)
        .ok_or(GptError::InvalidEntryArray)?
        / sector_size as usize;
    let array_end = partition_entries_lba
        .checked_add(array_sectors as u64)
        .ok_or(GptError::InvalidEntryArray)?;
    if partition_entries_lba == 0 || array_end > total_sectors {
        return Err(GptError::InvalidEntryArray);
    }

    Ok(GptHeader {
        current_lba,
        backup_lba,
        first_usable_lba,
        last_usable_lba,
        disk_guid,
        partition_entries_lba,
        partition_count,
        partition_entry_size,
    })
}

pub fn parse_partition_entry(
    entry: &[u8],
    header: &GptHeader,
) -> Result<Option<GptPartition>, GptError> {
    if entry.len() < GPT_ENTRY_MIN_SIZE as usize {
        return Err(GptError::InvalidEntrySize);
    }
    let mut type_guid = [0u8; 16];
    type_guid.copy_from_slice(&entry[..16]);
    if type_guid == [0; 16] {
        return Ok(None);
    }

    let mut unique_guid = [0u8; 16];
    unique_guid.copy_from_slice(&entry[16..32]);
    let first_lba = le_u64(entry, 32);
    let last_lba = le_u64(entry, 40);
    if first_lba < header.first_usable_lba
        || first_lba > last_lba
        || last_lba > header.last_usable_lba
    {
        return Err(GptError::InvalidPartitionRange);
    }

    let mut name = [0u16; GPT_NAME_CODE_UNITS];
    for (index, character) in name.iter_mut().enumerate() {
        let offset = 56 + index * 2;
        *character = u16::from_le_bytes([entry[offset], entry[offset + 1]]);
    }

    Ok(Some(GptPartition {
        type_guid,
        unique_guid,
        first_lba,
        last_lba,
        attributes: le_u64(entry, 48),
        name,
    }))
}

pub fn scan(device: &mut dyn BlockDevice) -> Result<GptTable, GptError> {
    let sector_size = device.sector_size();
    if sector_size < 512 {
        return Err(GptError::InvalidSectorSize);
    }
    let sector_size = sector_size as usize;
    let total_sectors = device.total_sectors();

    let mut header_sector = vec![0u8; sector_size];
    device.read_sectors(1, 1, &mut header_sector)?;
    let header = parse_header(&header_sector, sector_size as u32, total_sectors)?;

    let array_bytes = (header.partition_count as usize)
        .checked_mul(header.partition_entry_size as usize)
        .ok_or(GptError::InvalidEntryArray)?;
    let array_sectors = array_bytes
        .checked_add(sector_size - 1)
        .ok_or(GptError::InvalidEntryArray)?
        / sector_size;
    let read_bytes = array_sectors
        .checked_mul(sector_size)
        .ok_or(GptError::InvalidEntryArray)?;
    let read_count = u16::try_from(array_sectors).map_err(|_| GptError::InvalidEntryArray)?;
    let mut entries = vec![0u8; read_bytes];
    device.read_sectors(header.partition_entries_lba, read_count, &mut entries)?;

    let entry_size = header.partition_entry_size as usize;
    let mut partitions = Vec::new();
    for index in 0..header.partition_count as usize {
        let offset = index * entry_size;
        let end = offset + entry_size;
        if end > entries.len() {
            return Err(GptError::InvalidEntryArray);
        }
        if let Some(partition) = parse_partition_entry(&entries[offset..end], &header)? {
            partitions.push(partition);
        }
    }

    Ok(GptTable { header, partitions })
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    struct MemoryBlockDevice {
        data: Vec<u8>,
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

        fn write_sectors(&mut self, _lba: u64, _count: u16, _buf: &[u8]) -> Result<(), BlockError> {
            Err(BlockError::Device)
        }

        fn sector_size(&self) -> u32 {
            512
        }

        fn total_sectors(&self) -> u64 {
            (self.data.len() / 512) as u64
        }
    }

    fn blank_disk(sectors: usize) -> MemoryBlockDevice {
        MemoryBlockDevice {
            data: vec![0; sectors * 512],
        }
    }

    fn put_header(disk: &mut MemoryBlockDevice) {
        let header = &mut disk.data[512..1024];
        header[..8].copy_from_slice(GPT_SIGNATURE);
        header[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        header[12..16].copy_from_slice(&92u32.to_le_bytes());
        header[24..32].copy_from_slice(&1u64.to_le_bytes());
        header[32..40].copy_from_slice(&999u64.to_le_bytes());
        header[40..48].copy_from_slice(&34u64.to_le_bytes());
        header[48..56].copy_from_slice(&900u64.to_le_bytes());
        header[72..80].copy_from_slice(&2u64.to_le_bytes());
        header[80..84].copy_from_slice(&4u32.to_le_bytes());
        header[84..88].copy_from_slice(&128u32.to_le_bytes());
    }

    #[test]
    fn parses_header_and_partition_entries() {
        let mut disk = blank_disk(1_000);
        put_header(&mut disk);
        let entry = &mut disk.data[2 * 512..3 * 512];
        entry[..16].copy_from_slice(&[1; 16]);
        entry[16..32].copy_from_slice(&[2; 16]);
        entry[32..40].copy_from_slice(&100u64.to_le_bytes());
        entry[40..48].copy_from_slice(&199u64.to_le_bytes());
        entry[56..66].copy_from_slice(&[b'd', 0, b'a', 0, b't', 0, b'a', 0, 0, 0]);

        let table = scan(&mut disk).unwrap();
        assert_eq!(table.partitions.len(), 1);
        assert_eq!(table.partitions[0].first_lba, 100);
        assert_eq!(table.partitions[0].last_lba, 199);
        assert_eq!(table.partitions[0].name(), "data");
    }

    #[test]
    fn rejects_partition_outside_usable_range() {
        let mut disk = blank_disk(1_000);
        put_header(&mut disk);
        let entry = &mut disk.data[2 * 512..3 * 512];
        entry[..16].copy_from_slice(&[1; 16]);
        entry[32..40].copy_from_slice(&10u64.to_le_bytes());
        entry[40..48].copy_from_slice(&199u64.to_le_bytes());

        assert_eq!(scan(&mut disk), Err(GptError::InvalidPartitionRange));
    }
}
