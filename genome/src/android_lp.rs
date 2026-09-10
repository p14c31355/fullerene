//! Read-only Android logical-partition metadata and linear mappings.
//!
//! The on-disk format is the AOSP `liblp` format. This module intentionally
//! implements structural validation and mapping only; it never updates
//! geometry, metadata slots, extents, or partition contents.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::block::{BlockDevice, BlockError};

pub const LP_SECTOR_SIZE: u64 = 512;
const GEOMETRY_MAGIC: u32 = 0x616c_4467;
const GEOMETRY_BYTES: usize = 52;
const GEOMETRY_RESERVED_BYTES: u64 = 4096;
const METADATA_HEADER_MAGIC: u32 = 0x414c_5030;
const METADATA_MAJOR_VERSION: u16 = 10;
const METADATA_MINOR_VERSION_MAX: u16 = 2;
const METADATA_HEADER_MIN_BYTES: usize = 128;
const METADATA_HEADER_MAX_BYTES: usize = 256;
const METADATA_MAX_BYTES: usize = 16 * 1024 * 1024;
const PARTITION_ENTRY_BYTES: usize = 52;
const EXTENT_ENTRY_BYTES: usize = 24;
const GROUP_ENTRY_BYTES: usize = 48;
const BLOCK_DEVICE_ENTRY_BYTES: usize = 64;
const PARTITION_ATTR_MASK: u32 = 0x0f;
const PARTITION_ATTR_SLOT_SUFFIXED: u32 = 1 << 1;
const BLOCK_DEVICE_SLOT_SUFFIXED: u32 = 1 << 0;
const TARGET_LINEAR: u32 = 0;
const TARGET_ZERO: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LpError {
    Device(BlockError),
    InvalidChecksum,
    InvalidSectorSize,
    InvalidGeometry,
    InvalidMetadata,
    InvalidTable,
    InvalidPartition,
    InvalidExtent,
    PartitionNotFound,
    UnsupportedExtent,
}

impl From<BlockError> for LpError {
    fn from(error: BlockError) -> Self {
        Self::Device(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LpGeometry {
    pub metadata_max_size: u32,
    pub metadata_slot_count: u32,
    pub logical_block_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LpPartition {
    pub name: String,
    pub attributes: u32,
    pub first_extent_index: u32,
    pub num_extents: u32,
    pub group_index: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LpExtent {
    pub num_sectors: u64,
    pub target_type: u32,
    pub target_data: u64,
    pub target_source: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LpBlockDevice {
    pub first_logical_sector: u64,
    pub size_bytes: u64,
    pub partition_name: String,
    pub flags: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LpMetadata {
    pub geometry: LpGeometry,
    pub partitions: Vec<LpPartition>,
    pub extents: Vec<LpExtent>,
    pub block_devices: Vec<LpBlockDevice>,
}

fn le_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn le_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn le_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn fixed_ascii(bytes: &[u8]) -> String {
    let mut result = String::new();
    for &byte in bytes {
        if byte == 0 {
            break;
        }
        result.push(if byte.is_ascii() {
            byte as char
        } else {
            '\u{fffd}'
        });
    }
    result
}

struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffered: usize,
    length: u64,
}

impl Sha256 {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    const ROUND_CONSTANTS: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    fn new() -> Self {
        Self {
            state: Self::INITIAL,
            buffer: [0; 64],
            buffered: 0,
            length: 0,
        }
    }

    fn process_block(&mut self, block: &[u8]) {
        let mut words = [0u32; 64];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes(block[offset..offset + 4].try_into().unwrap());
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }

        let mut working = self.state;
        for index in 0..64 {
            let sum1 = working[4].rotate_right(6)
                ^ working[4].rotate_right(11)
                ^ working[4].rotate_right(25);
            let choice = (working[4] & working[5]) ^ ((!working[4]) & working[6]);
            let temp1 = working[7]
                .wrapping_add(sum1)
                .wrapping_add(choice)
                .wrapping_add(Self::ROUND_CONSTANTS[index])
                .wrapping_add(words[index]);
            let sum0 = working[0].rotate_right(2)
                ^ working[0].rotate_right(13)
                ^ working[0].rotate_right(22);
            let majority =
                (working[0] & working[1]) ^ (working[0] & working[2]) ^ (working[1] & working[2]);
            let temp2 = sum0.wrapping_add(majority);
            working[7] = working[6];
            working[6] = working[5];
            working[5] = working[4];
            working[4] = working[3].wrapping_add(temp1);
            working[3] = working[2];
            working[2] = working[1];
            working[1] = working[0];
            working[0] = temp1.wrapping_add(temp2);
        }
        for (state, value) in self.state.iter_mut().zip(working) {
            *state = state.wrapping_add(value);
        }
    }

    fn update(&mut self, mut bytes: &[u8]) {
        self.length = self.length.wrapping_add(bytes.len() as u64);
        if self.buffered != 0 {
            let take = (64 - self.buffered).min(bytes.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&bytes[..take]);
            self.buffered += take;
            bytes = &bytes[take..];
            if self.buffered == 64 {
                let block = self.buffer;
                self.process_block(&block);
                self.buffered = 0;
            }
        }
        while bytes.len() >= 64 {
            self.process_block(&bytes[..64]);
            bytes = &bytes[64..];
        }
        if !bytes.is_empty() {
            self.buffer[..bytes.len()].copy_from_slice(bytes);
            self.buffered = bytes.len();
        }
    }

    fn finish(mut self) -> [u8; 32] {
        let bit_length = self.length.wrapping_mul(8);
        self.update(&[0x80]);
        while self.buffered != 56 {
            self.update(&[0]);
        }
        self.buffer[56..64].copy_from_slice(&bit_length.to_be_bytes());
        let block = self.buffer;
        self.process_block(&block);

        let mut output = [0u8; 32];
        for (index, value) in self.state.iter().enumerate() {
            output[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
        }
        output
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finish()
}

fn sha256_with_zero_range(bytes: &[u8], zero_start: usize, zero_len: usize) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(&bytes[..zero_start]);
    let zeros = [0u8; 64];
    let mut remaining = zero_len;
    while remaining != 0 {
        let take = remaining.min(zeros.len());
        hasher.update(&zeros[..take]);
        remaining -= take;
    }
    hasher.update(&bytes[zero_start + zero_len..]);
    hasher.finish()
}

pub fn parse_geometry(bytes: &[u8]) -> Result<LpGeometry, LpError> {
    if bytes.len() < GEOMETRY_BYTES || le_u32(bytes, 0) != GEOMETRY_MAGIC {
        return Err(LpError::InvalidGeometry);
    }
    let expected_checksum = sha256_with_zero_range(&bytes[..GEOMETRY_BYTES], 8, 32);
    if bytes[8..40] != expected_checksum {
        return Err(LpError::InvalidChecksum);
    }
    let struct_size = le_u32(bytes, 4) as usize;
    let metadata_max_size = le_u32(bytes, 40);
    let metadata_slot_count = le_u32(bytes, 44);
    let logical_block_size = le_u32(bytes, 48);
    if struct_size != GEOMETRY_BYTES
        || metadata_max_size == 0
        || metadata_max_size as usize > METADATA_MAX_BYTES
        || !metadata_max_size.is_multiple_of(LP_SECTOR_SIZE as u32)
        || metadata_slot_count == 0
        || metadata_slot_count > 8
        || logical_block_size < LP_SECTOR_SIZE as u32
        || !logical_block_size.is_multiple_of(LP_SECTOR_SIZE as u32)
    {
        return Err(LpError::InvalidGeometry);
    }
    Ok(LpGeometry {
        metadata_max_size,
        metadata_slot_count,
        logical_block_size,
    })
}

fn validate_table(
    tables_size: usize,
    offset: u32,
    count: u32,
    entry_size: u32,
    minimum_entry_size: usize,
) -> Result<(usize, usize), LpError> {
    let offset = offset as usize;
    let entry_size = entry_size as usize;
    let count = count as usize;
    let bytes = count.checked_mul(entry_size).ok_or(LpError::InvalidTable)?;
    let end = offset.checked_add(bytes).ok_or(LpError::InvalidTable)?;
    if entry_size < minimum_entry_size || end > tables_size {
        return Err(LpError::InvalidTable);
    }
    Ok((offset, entry_size))
}

pub fn parse_metadata(geometry: LpGeometry, bytes: &[u8]) -> Result<LpMetadata, LpError> {
    if bytes.len() < METADATA_HEADER_MAX_BYTES || le_u32(bytes, 0) != METADATA_HEADER_MAGIC {
        return Err(LpError::InvalidMetadata);
    }
    let expected_header_checksum =
        sha256_with_zero_range(&bytes[..METADATA_HEADER_MAX_BYTES], 12, 32);
    if bytes[12..44] != expected_header_checksum {
        return Err(LpError::InvalidChecksum);
    }
    let major = le_u16(bytes, 4);
    let minor = le_u16(bytes, 6);
    let header_size = le_u32(bytes, 8) as usize;
    let tables_size = le_u32(bytes, 44) as usize;
    if major != METADATA_MAJOR_VERSION
        || minor > METADATA_MINOR_VERSION_MAX
        || !matches!(
            header_size,
            METADATA_HEADER_MIN_BYTES | METADATA_HEADER_MAX_BYTES
        )
        || tables_size > geometry.metadata_max_size as usize
        || METADATA_HEADER_MAX_BYTES
            .checked_add(tables_size)
            .is_none_or(|end| end > bytes.len())
    {
        return Err(LpError::InvalidMetadata);
    }

    // AOSP serializes the current 256-byte header area even when header_size
    // is 128 for the legacy 1.0 checksum format. Table offsets are relative to
    // the end of this serialized header area.
    let tables = &bytes[METADATA_HEADER_MAX_BYTES..METADATA_HEADER_MAX_BYTES + tables_size];
    if bytes[48..80] != sha256(tables) {
        return Err(LpError::InvalidChecksum);
    }
    let (partition_offset, partition_size) = validate_table(
        tables_size,
        le_u32(bytes, 80),
        le_u32(bytes, 84),
        le_u32(bytes, 88),
        PARTITION_ENTRY_BYTES,
    )?;
    let (extent_offset, extent_size) = validate_table(
        tables_size,
        le_u32(bytes, 92),
        le_u32(bytes, 96),
        le_u32(bytes, 100),
        EXTENT_ENTRY_BYTES,
    )?;
    let (group_offset, group_size) = validate_table(
        tables_size,
        le_u32(bytes, 104),
        le_u32(bytes, 108),
        le_u32(bytes, 112),
        GROUP_ENTRY_BYTES,
    )?;
    let (block_device_offset, block_device_size) = validate_table(
        tables_size,
        le_u32(bytes, 116),
        le_u32(bytes, 120),
        le_u32(bytes, 124),
        BLOCK_DEVICE_ENTRY_BYTES,
    )?;

    let partition_count = le_u32(bytes, 84) as usize;
    let extent_count = le_u32(bytes, 96) as usize;
    let group_count = le_u32(bytes, 108) as usize;
    let block_device_count = le_u32(bytes, 120) as usize;
    if partition_count > 4096
        || extent_count > 16_384
        || group_count > 4096
        || block_device_count > 32
    {
        return Err(LpError::InvalidTable);
    }

    let mut partitions = Vec::with_capacity(partition_count);
    for index in 0..partition_count {
        let offset = partition_offset + index * partition_size;
        let entry = &tables[offset..offset + partition_size];
        let attributes = le_u32(entry, 36);
        let first_extent_index = le_u32(entry, 40);
        let num_extents = le_u32(entry, 44);
        let group_index = le_u32(entry, 48);
        if attributes & !PARTITION_ATTR_MASK != 0
            || num_extents == 0
            || group_index as usize >= group_count
            || (first_extent_index as usize)
                .checked_add(num_extents as usize)
                .is_none_or(|end| end > extent_count)
        {
            return Err(LpError::InvalidPartition);
        }
        let name = fixed_ascii(&entry[..36]);
        if name.is_empty() {
            return Err(LpError::InvalidPartition);
        }
        partitions.push(LpPartition {
            name,
            attributes,
            first_extent_index,
            num_extents,
            group_index,
        });
    }

    let mut extents = Vec::with_capacity(extent_count);
    for index in 0..extent_count {
        let offset = extent_offset + index * extent_size;
        let entry = &tables[offset..offset + extent_size];
        let extent = LpExtent {
            num_sectors: le_u64(entry, 0),
            target_type: le_u32(entry, 8),
            target_data: le_u64(entry, 12),
            target_source: le_u32(entry, 20),
        };
        if extent.num_sectors == 0
            || !matches!(extent.target_type, TARGET_LINEAR | TARGET_ZERO)
            || (extent.target_type == TARGET_ZERO
                && (extent.target_data != 0 || extent.target_source != 0))
            || (extent.target_type == TARGET_LINEAR
                && extent.target_source as usize >= block_device_count)
        {
            return Err(LpError::InvalidExtent);
        }
        extents.push(extent);
    }

    for index in 0..group_count {
        let offset = group_offset + index * group_size;
        let entry = &tables[offset..offset + group_size];
        if fixed_ascii(&entry[..36]).is_empty() {
            return Err(LpError::InvalidTable);
        }
    }

    let mut block_devices = Vec::with_capacity(block_device_count);
    for index in 0..block_device_count {
        let offset = block_device_offset + index * block_device_size;
        let entry = &tables[offset..offset + block_device_size];
        let device = LpBlockDevice {
            first_logical_sector: le_u64(entry, 0),
            size_bytes: le_u64(entry, 16),
            partition_name: fixed_ascii(&entry[24..60]),
            flags: le_u32(entry, 60),
        };
        if device.partition_name.is_empty()
            || device.size_bytes < LP_SECTOR_SIZE
            || !device.size_bytes.is_multiple_of(LP_SECTOR_SIZE)
        {
            return Err(LpError::InvalidTable);
        }
        block_devices.push(device);
    }
    if block_devices.is_empty() {
        return Err(LpError::InvalidTable);
    }
    let metadata_region_sectors = 8u64
        .checked_add(16)
        .and_then(|value| {
            value.checked_add(
                geometry.metadata_max_size as u64 * geometry.metadata_slot_count as u64 * 2
                    / LP_SECTOR_SIZE,
            )
        })
        .ok_or(LpError::InvalidTable)?;
    if block_devices[0].first_logical_sector < metadata_region_sectors
        || block_devices[0]
            .first_logical_sector
            .checked_mul(LP_SECTOR_SIZE)
            .is_none_or(|start| start > block_devices[0].size_bytes)
    {
        return Err(LpError::InvalidTable);
    }

    for extent in &extents {
        if extent.target_type != TARGET_LINEAR {
            continue;
        }
        let device = &block_devices[extent.target_source as usize];
        let device_sectors = device.size_bytes / LP_SECTOR_SIZE;
        if extent
            .target_data
            .checked_add(extent.num_sectors)
            .is_none_or(|end| end > device_sectors)
        {
            return Err(LpError::InvalidExtent);
        }
    }

    Ok(LpMetadata {
        geometry,
        partitions,
        extents,
        block_devices,
    })
}

fn read_sectors(
    device: &mut dyn BlockDevice,
    start_byte: u64,
    byte_count: usize,
) -> Result<Vec<u8>, LpError> {
    if device.sector_size() != LP_SECTOR_SIZE as u32 || !start_byte.is_multiple_of(LP_SECTOR_SIZE) {
        return Err(LpError::InvalidSectorSize);
    }
    let sectors = byte_count
        .checked_add(LP_SECTOR_SIZE as usize - 1)
        .ok_or(LpError::InvalidMetadata)?
        / LP_SECTOR_SIZE as usize;
    let read_bytes = sectors
        .checked_mul(LP_SECTOR_SIZE as usize)
        .ok_or(LpError::InvalidMetadata)?;
    let count = u16::try_from(sectors).map_err(|_| LpError::InvalidMetadata)?;
    let mut result = vec![0u8; read_bytes];
    device.read_sectors(start_byte / LP_SECTOR_SIZE, count, &mut result)?;
    result.truncate(byte_count);
    Ok(result)
}

fn read_metadata_at(
    device: &mut dyn BlockDevice,
    geometry: LpGeometry,
    byte_offset: u64,
) -> Result<LpMetadata, LpError> {
    let prefix = read_sectors(device, byte_offset, LP_SECTOR_SIZE as usize)?;
    if prefix.len() < METADATA_HEADER_MIN_BYTES {
        return Err(LpError::InvalidMetadata);
    }
    let _header_size = le_u32(&prefix, 8) as usize;
    let tables_size = le_u32(&prefix, 44) as usize;
    let total = METADATA_HEADER_MAX_BYTES
        .checked_add(tables_size)
        .ok_or(LpError::InvalidMetadata)?;
    if total > geometry.metadata_max_size as usize || total > METADATA_MAX_BYTES {
        return Err(LpError::InvalidMetadata);
    }
    let bytes = read_sectors(device, byte_offset, total)?;
    parse_metadata(geometry, &bytes)
}

pub fn read_metadata(mut device: Box<dyn BlockDevice>, slot: u32) -> Result<LpMetadata, LpError> {
    if device.sector_size() != LP_SECTOR_SIZE as u32 {
        return Err(LpError::InvalidSectorSize);
    }
    let primary_geometry = read_sectors(
        &mut *device,
        GEOMETRY_RESERVED_BYTES,
        GEOMETRY_RESERVED_BYTES as usize,
    )
    .and_then(|bytes| parse_geometry(&bytes));
    let geometry = match primary_geometry {
        Ok(geometry) => geometry,
        Err(_) => {
            let bytes = read_sectors(
                &mut *device,
                GEOMETRY_RESERVED_BYTES * 2,
                GEOMETRY_RESERVED_BYTES as usize,
            )?;
            parse_geometry(&bytes)?
        }
    };
    if slot >= geometry.metadata_slot_count {
        return Err(LpError::InvalidMetadata);
    }

    let metadata_size = geometry.metadata_max_size as u64;
    let primary_offset = GEOMETRY_RESERVED_BYTES * 3 + metadata_size * slot as u64;
    let backup_offset = GEOMETRY_RESERVED_BYTES * 3
        + metadata_size * geometry.metadata_slot_count as u64
        + metadata_size * slot as u64;
    let mut metadata = read_metadata_at(&mut *device, geometry, primary_offset)
        .or_else(|_| read_metadata_at(&mut *device, geometry, backup_offset))?;
    adjust_for_slot(&mut metadata, slot)?;
    Ok(metadata)
}

fn adjust_for_slot(metadata: &mut LpMetadata, slot: u32) -> Result<(), LpError> {
    let suffix = match slot {
        0 => "_a",
        1 => "_b",
        _ => return Ok(()),
    };
    for partition in &mut metadata.partitions {
        if partition.attributes & PARTITION_ATTR_SLOT_SUFFIXED == 0 {
            continue;
        }
        if partition.name.len() + suffix.len() > 36 {
            return Err(LpError::InvalidPartition);
        }
        partition.name.push_str(suffix);
        partition.attributes &= !PARTITION_ATTR_SLOT_SUFFIXED;
    }
    for device in &mut metadata.block_devices {
        if device.flags & BLOCK_DEVICE_SLOT_SUFFIXED == 0 {
            continue;
        }
        if device.partition_name.len() + suffix.len() > 36 {
            return Err(LpError::InvalidTable);
        }
        device.partition_name.push_str(suffix);
        device.flags &= !BLOCK_DEVICE_SLOT_SUFFIXED;
    }
    Ok(())
}

pub struct LinearBlockDevice {
    inner: Box<dyn BlockDevice>,
    extents: Vec<LpExtent>,
    total_sectors: u64,
}

impl LinearBlockDevice {
    pub fn new(
        metadata: &LpMetadata,
        inner: Box<dyn BlockDevice>,
        name: &str,
    ) -> Result<Self, LpError> {
        if inner.sector_size() != LP_SECTOR_SIZE as u32 {
            return Err(LpError::InvalidSectorSize);
        }
        let partition = metadata
            .partitions
            .iter()
            .find(|partition| partition.name == name)
            .ok_or(LpError::PartitionNotFound)?;
        if partition.attributes & (1 << 3) != 0 || partition.num_extents == 0 {
            return Err(LpError::InvalidPartition);
        }
        let mut extents = Vec::with_capacity(partition.num_extents as usize);
        let mut total_sectors = 0u64;
        for index in 0..partition.num_extents as usize {
            let extent_index = (partition.first_extent_index as usize)
                .checked_add(index)
                .ok_or(LpError::InvalidExtent)?;
            let extent = *metadata
                .extents
                .get(extent_index)
                .ok_or(LpError::InvalidExtent)?;
            if extent.target_type != TARGET_LINEAR || extent.target_source != 0 {
                return Err(LpError::UnsupportedExtent);
            }
            let end = extent
                .target_data
                .checked_add(extent.num_sectors)
                .ok_or(LpError::InvalidExtent)?;
            if end > inner.total_sectors() {
                return Err(LpError::InvalidExtent);
            }
            total_sectors = total_sectors
                .checked_add(extent.num_sectors)
                .ok_or(LpError::InvalidExtent)?;
            extents.push(extent);
        }
        Ok(Self {
            inner,
            extents,
            total_sectors,
        })
    }
}

impl BlockDevice for LinearBlockDevice {
    fn read_sectors(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
        let byte_count = (count as usize)
            .checked_mul(LP_SECTOR_SIZE as usize)
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
        if end > self.total_sectors {
            return Err(BlockError::LbaOverflow);
        }
        let mut logical = lba;
        let mut remaining = count as u64;
        let mut output = 0usize;
        let mut extent_index = 0usize;
        while extent_index < self.extents.len() && logical >= self.extents[extent_index].num_sectors
        {
            logical -= self.extents[extent_index].num_sectors;
            extent_index += 1;
        }
        while remaining != 0 && extent_index < self.extents.len() {
            let extent = self.extents[extent_index];
            if logical >= extent.num_sectors {
                logical -= extent.num_sectors;
                extent_index += 1;
                continue;
            }
            let take = remaining
                .min(extent.num_sectors - logical)
                .min(u16::MAX as u64) as u16;
            let take_bytes = take as usize * LP_SECTOR_SIZE as usize;
            self.inner.read_sectors(
                extent.target_data + logical,
                take,
                &mut buf[output..output + take_bytes],
            )?;
            output += take_bytes;
            remaining -= take as u64;
            logical += take as u64;
            if logical == extent.num_sectors {
                logical = 0;
                extent_index += 1;
            }
        }
        (remaining == 0)
            .then_some(())
            .ok_or(BlockError::SectorNotFound)
    }

    fn write_sectors(&mut self, _lba: u64, _count: u16, _buf: &[u8]) -> Result<(), BlockError> {
        Err(BlockError::Device)
    }

    fn sector_size(&self) -> u32 {
        LP_SECTOR_SIZE as u32
    }

    fn total_sectors(&self) -> u64 {
        self.total_sectors
    }
}

#[cfg(test)]
mod tests {
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

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn metadata_fixture_bytes() -> (LpGeometry, Vec<u8>) {
        let geometry = LpGeometry {
            metadata_max_size: 4096,
            metadata_slot_count: 1,
            logical_block_size: 4096,
        };
        let tables_size = PARTITION_ENTRY_BYTES
            + EXTENT_ENTRY_BYTES
            + GROUP_ENTRY_BYTES
            + BLOCK_DEVICE_ENTRY_BYTES;
        let mut bytes = vec![0u8; METADATA_HEADER_MAX_BYTES + tables_size];
        put_u32(&mut bytes, 0, METADATA_HEADER_MAGIC);
        bytes[4..6].copy_from_slice(&METADATA_MAJOR_VERSION.to_le_bytes());
        bytes[6..8].copy_from_slice(&0u16.to_le_bytes());
        put_u32(&mut bytes, 8, METADATA_HEADER_MAX_BYTES as u32);
        put_u32(&mut bytes, 44, tables_size as u32);
        put_u32(&mut bytes, 80, 0);
        put_u32(&mut bytes, 84, 1);
        put_u32(&mut bytes, 88, PARTITION_ENTRY_BYTES as u32);
        put_u32(&mut bytes, 92, PARTITION_ENTRY_BYTES as u32);
        put_u32(&mut bytes, 96, 1);
        put_u32(&mut bytes, 100, EXTENT_ENTRY_BYTES as u32);
        put_u32(
            &mut bytes,
            104,
            (PARTITION_ENTRY_BYTES + EXTENT_ENTRY_BYTES) as u32,
        );
        put_u32(&mut bytes, 108, 1);
        put_u32(&mut bytes, 112, GROUP_ENTRY_BYTES as u32);
        put_u32(
            &mut bytes,
            116,
            (PARTITION_ENTRY_BYTES + EXTENT_ENTRY_BYTES + GROUP_ENTRY_BYTES) as u32,
        );
        put_u32(&mut bytes, 120, 1);
        put_u32(&mut bytes, 124, BLOCK_DEVICE_ENTRY_BYTES as u32);

        let table = METADATA_HEADER_MAX_BYTES;
        bytes[table..table + 6].copy_from_slice(b"system");
        put_u32(&mut bytes, table + 44, 1);
        let extent = table + PARTITION_ENTRY_BYTES;
        put_u64(&mut bytes, extent, 128);
        put_u32(&mut bytes, extent + 8, TARGET_LINEAR);
        put_u64(&mut bytes, extent + 12, 256);
        let group = extent + EXTENT_ENTRY_BYTES;
        bytes[group..group + 7].copy_from_slice(b"default");
        let block = group + GROUP_ENTRY_BYTES;
        put_u64(&mut bytes, block, 64);
        put_u64(&mut bytes, block + 16, 1024 * 512);
        bytes[block + 24..block + 29].copy_from_slice(b"super");

        let table_checksum = sha256(&bytes[METADATA_HEADER_MAX_BYTES..]);
        bytes[48..80].copy_from_slice(&table_checksum);
        let header_checksum = sha256_with_zero_range(&bytes[..METADATA_HEADER_MAX_BYTES], 12, 32);
        bytes[12..44].copy_from_slice(&header_checksum);

        (geometry, bytes)
    }

    fn metadata_fixture() -> LpMetadata {
        let (geometry, bytes) = metadata_fixture_bytes();
        parse_metadata(geometry, &bytes).unwrap()
    }

    #[test]
    fn sha256_matches_known_digest() {
        assert_eq!(
            sha256(b"abc"),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
    }

    #[test]
    fn parses_metadata_and_maps_linear_partition() {
        let metadata = metadata_fixture();
        assert_eq!(metadata.partitions[0].name, "system");
        assert_eq!(metadata.extents[0].target_data, 256);

        let mut data = vec![0u8; 1024 * 512];
        data[256 * 512..257 * 512].fill(0xA5);
        let mut logical =
            LinearBlockDevice::new(&metadata, Box::new(MemoryBlockDevice { data }), "system")
                .unwrap();
        let mut sector = [0u8; 512];
        logical.read_sectors(0, 1, &mut sector).unwrap();
        assert_eq!(sector, [0xA5; 512]);
        assert_eq!(
            logical.write_sectors(0, 1, &sector),
            Err(BlockError::Device)
        );
    }

    #[test]
    fn reads_geometry_and_metadata_at_aosp_offsets() {
        let (geometry, metadata_bytes) = metadata_fixture_bytes();
        let mut geometry_bytes = vec![0u8; GEOMETRY_RESERVED_BYTES as usize];
        put_u32(&mut geometry_bytes, 0, GEOMETRY_MAGIC);
        put_u32(&mut geometry_bytes, 4, GEOMETRY_BYTES as u32);
        put_u32(&mut geometry_bytes, 40, geometry.metadata_max_size);
        put_u32(&mut geometry_bytes, 44, geometry.metadata_slot_count);
        put_u32(&mut geometry_bytes, 48, geometry.logical_block_size);
        let checksum = sha256_with_zero_range(&geometry_bytes[..GEOMETRY_BYTES], 8, 32);
        geometry_bytes[8..40].copy_from_slice(&checksum);

        let mut data = vec![0u8; 40 * 512];
        data[GEOMETRY_RESERVED_BYTES as usize..GEOMETRY_RESERVED_BYTES as usize * 2]
            .copy_from_slice(&geometry_bytes);
        let metadata_start = GEOMETRY_RESERVED_BYTES as usize * 3;
        data[metadata_start..metadata_start + metadata_bytes.len()]
            .copy_from_slice(&metadata_bytes);

        let metadata = read_metadata(Box::new(MemoryBlockDevice { data }), 0).unwrap();
        assert_eq!(metadata.geometry, geometry);
        assert_eq!(metadata.partitions[0].name, "system");
    }

    #[test]
    fn rejects_metadata_with_out_of_range_extent() {
        let mut metadata = metadata_fixture();
        metadata.extents[0].target_data = u64::MAX;
        let result = LinearBlockDevice::new(
            &metadata,
            Box::new(MemoryBlockDevice {
                data: vec![0; 1024 * 512],
            }),
            "system",
        );
        assert!(matches!(result, Err(LpError::InvalidExtent)));
    }
}
