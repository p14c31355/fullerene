//! Bounded read-only F2FS access for Android userdata partitions.
//!
//! This reader intentionally implements the stable, uncompressed 4 KiB F2FS
//! layout used by ordinary Android userdata volumes.  It does not replay a
//! dirty checkpoint, decrypt filenames/data, decode compressed clusters, or
//! perform any mutation.  Unsupported on-disk features are rejected during
//! construction rather than being silently misread.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::block::BlockDevice;
use crate::fs::FsError;
use crate::vfs::{
    FileDescriptor, FileMetadata, FileSystem, FileSystemCapabilities, InodeType, VNode,
};

const F2FS_SUPER_OFFSET: u64 = 1024;
const F2FS_SUPER_BYTES: usize = 3072;
const F2FS_BLOCK_SIZE: u64 = 4096;
const F2FS_BLOCK_BITS: u32 = 12;
const F2FS_BLOCKS_PER_SEGMENT: u64 = 512;
const F2FS_MAGIC: u32 = 0xf2f5_2010;
const F2FS_SUPER_MAGIC_SEED: u32 = F2FS_MAGIC;
const F2FS_SUPER_CRC_OFFSET: usize = 3068;
const F2FS_CHECKPOINT_CRC_OFFSET: usize = 4092;
const F2FS_CHECKPOINT_MIN_CRC_OFFSET: usize = 192;

const F2FS_FEATURE_ENCRYPT: u32 = 0x0000_0001;
const F2FS_FEATURE_BLKZONED: u32 = 0x0000_0002;
const F2FS_FEATURE_EXTRA_ATTR: u32 = 0x0000_0008;
const F2FS_FEATURE_FLEXIBLE_INLINE_XATTR: u32 = 0x0000_0040;
const F2FS_FEATURE_SB_CHKSUM: u32 = 0x0000_0800;
const F2FS_FEATURE_CASEFOLD: u32 = 0x0000_1000;
const F2FS_FEATURE_COMPRESSION: u32 = 0x0000_2000;
const F2FS_FEATURE_PACKED_SSA: u32 = 0x0001_0000;

const F2FS_CP_UMOUNT_FLAG: u32 = 0x0000_0001;
const F2FS_CP_ERROR_FLAG: u32 = 0x0000_0008;
const F2FS_CP_LARGE_NAT_BITMAP_FLAG: u32 = 0x0000_0400;
const F2FS_CP_DISABLED_FLAG: u32 = 0x0000_1000;

const F2FS_INLINE_XATTR: u8 = 0x01;
const F2FS_INLINE_DATA: u8 = 0x02;
const F2FS_INLINE_DENTRY: u8 = 0x04;
const F2FS_EXTRA_ATTR: u8 = 0x20;

const F2FS_COMPR_FL: u32 = 0x0000_0004;
const F2FS_CASEFOLD_FL: u32 = 0x4000_0000;

const S_IFMT: u16 = 0xf000;
const S_IFREG: u16 = 0x8000;
const S_IFDIR: u16 = 0x4000;
const S_IFLNK: u16 = 0xa000;

const OFFSET_OF_END_OF_I_EXT: usize = 360;
const DEF_ADDRS_PER_INODE: usize = (4096 - 360 - 20 - 24) / 4;
const DEF_ADDRS_PER_BLOCK: usize = (4096 - 24) / 4;
const DEF_NIDS_PER_INODE: usize = 5;
const NAT_ENTRY_SIZE: usize = 9;
const NAT_ENTRY_PER_BLOCK: u64 = (4096 / NAT_ENTRY_SIZE) as u64;
const F2FS_SLOT_LEN: usize = 8;
const F2FS_DENTRY_SIZE: usize = 11;
const F2FS_DENTRY_COUNT: usize = (8 * 4096) / ((F2FS_DENTRY_SIZE + F2FS_SLOT_LEN) * 8 + 1);
const F2FS_DENTRY_BITMAP_SIZE: usize = (F2FS_DENTRY_COUNT + 7) / 8;
const F2FS_DENTRY_RESERVED: usize =
    4096 - (F2FS_DENTRY_SIZE + F2FS_SLOT_LEN) * F2FS_DENTRY_COUNT - F2FS_DENTRY_BITMAP_SIZE;
const F2FS_DENTRY_ENTRY_OFFSET: usize = F2FS_DENTRY_BITMAP_SIZE + F2FS_DENTRY_RESERVED;
const F2FS_DENTRY_NAME_OFFSET: usize =
    F2FS_DENTRY_ENTRY_OFFSET + F2FS_DENTRY_SIZE * F2FS_DENTRY_COUNT;

const MAX_PATH_COMPONENTS: usize = 1024;
const MAX_SYMLINK_DEPTH: u32 = 8;
const MAX_SYMLINK_BYTES: u64 = 4096;
const MAX_DIR_ENTRIES: usize = 131_072;
const MAX_DIRECTORY_BLOCKS: u64 = 131_072;

fn le_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn le_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn le_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn crc32(mut crc: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    crc
}

#[derive(Clone, Copy)]
struct Checkpoint {
    version: u64,
    flags: u32,
    nat_bitmap_bytes: u32,
    nat_bitmap_offset: usize,
}

#[derive(Clone)]
struct InodeRecord {
    nid: u32,
    mode: u16,
    uid: u32,
    gid: u32,
    inline_flags: u8,
    size: u64,
    extra_isize: usize,
    address_words: usize,
    node: Vec<u8>,
}

impl InodeRecord {
    fn kind(&self) -> Result<InodeType, FsError> {
        match self.mode & S_IFMT {
            S_IFREG => Ok(InodeType::File),
            S_IFDIR => Ok(InodeType::Directory),
            S_IFLNK => Ok(InodeType::Symlink),
            _ => Err(FsError::NotSupported),
        }
    }

    fn inline_area_offset(&self) -> Result<usize, FsError> {
        if self.inline_flags & (F2FS_INLINE_DATA | F2FS_INLINE_DENTRY) == 0 {
            return Err(FsError::NotSupported);
        }
        OFFSET_OF_END_OF_I_EXT
            .checked_add(self.extra_isize)
            .and_then(|offset| offset.checked_add(4))
            .ok_or(FsError::InvalidInput)
    }

    fn inline_area_bytes(&self) -> Result<usize, FsError> {
        self.address_words
            .checked_sub(1)
            .and_then(|words| words.checked_mul(4))
            .ok_or(FsError::InvalidInput)
    }
}

#[derive(Clone)]
struct F2fsHandle {
    fd: u32,
    nid: u32,
    offset: u64,
}

#[derive(Clone)]
struct DirectoryEntry {
    nid: u32,
    name: String,
}

pub struct F2fsFileSystem {
    device: Box<dyn BlockDevice>,
    total_blocks: u64,
    block_count: u64,
    cp_blkaddr: u64,
    nat_blkaddr: u64,
    main_blkaddr: u64,
    segment_count_nat: u64,
    segment_count_main: u64,
    extra_attr: bool,
    flexible_inline_xattr: bool,
    root_nid: u32,
    checkpoint: Checkpoint,
    handles: Vec<F2fsHandle>,
    next_fd: u32,
}

impl F2fsFileSystem {
    pub fn new(mut device: Box<dyn BlockDevice>) -> Result<Self, FsError> {
        let sector_size = device.sector_size();
        if sector_size < 512 || !sector_size.is_multiple_of(512) || sector_size > 16_384 {
            return Err(FsError::InvalidInput);
        }
        let total_bytes = device
            .total_sectors()
            .checked_mul(sector_size as u64)
            .ok_or(FsError::InvalidInput)?;
        let total_blocks = total_bytes / F2FS_BLOCK_SIZE;
        if total_blocks == 0 {
            return Err(FsError::UnexpectedEof);
        }

        let mut superblock = vec![0u8; F2FS_SUPER_BYTES];
        Self::read_bytes_from(&mut *device, F2FS_SUPER_OFFSET, &mut superblock)?;
        if le_u32(&superblock, 0) != F2FS_MAGIC {
            return Err(FsError::InvalidInput);
        }

        let log_sector_size = le_u32(&superblock, 8);
        let log_sectors_per_block = le_u32(&superblock, 12);
        if !(9..=14).contains(&log_sector_size)
            || log_sector_size.checked_add(log_sectors_per_block) != Some(F2FS_BLOCK_BITS)
            || le_u32(&superblock, 16) != F2FS_BLOCK_BITS
            || le_u32(&superblock, 20) != 9
        {
            return Err(FsError::NotSupported);
        }
        if F2FS_BLOCK_SIZE % sector_size as u64 != 0 {
            return Err(FsError::InvalidInput);
        }

        let feature = le_u32(&superblock, 2180);
        if feature & F2FS_FEATURE_ENCRYPT != 0
            || feature & F2FS_FEATURE_BLKZONED != 0
            || feature & F2FS_FEATURE_CASEFOLD != 0
            || feature & F2FS_FEATURE_COMPRESSION != 0
            || feature & F2FS_FEATURE_PACKED_SSA != 0
        {
            return Err(FsError::NotSupported);
        }
        if feature & F2FS_FEATURE_SB_CHKSUM != 0 {
            if le_u32(&superblock, 32) as usize != F2FS_SUPER_CRC_OFFSET
                || le_u32(&superblock, F2FS_SUPER_CRC_OFFSET)
                    != crc32(F2FS_SUPER_MAGIC_SEED, &superblock[..F2FS_SUPER_CRC_OFFSET])
            {
                return Err(FsError::InvalidInput);
            }
        }

        let block_count = le_u64(&superblock, 36);
        let segment_count = le_u32(&superblock, 48) as u64;
        let segment_count_ckpt = le_u32(&superblock, 52) as u64;
        let segment_count_sit = le_u32(&superblock, 56) as u64;
        let segment_count_nat = le_u32(&superblock, 60) as u64;
        let segment_count_ssa = le_u32(&superblock, 64) as u64;
        let segment_count_main = le_u32(&superblock, 68) as u64;
        let segment0_blkaddr = le_u32(&superblock, 72) as u64;
        let cp_blkaddr = le_u32(&superblock, 76) as u64;
        let sit_blkaddr = le_u32(&superblock, 80) as u64;
        let nat_blkaddr = le_u32(&superblock, 84) as u64;
        let ssa_blkaddr = le_u32(&superblock, 88) as u64;
        let main_blkaddr = le_u32(&superblock, 92) as u64;
        let root_nid = le_u32(&superblock, 96);
        if block_count == 0
            || block_count > total_blocks
            || segment_count == 0
            || segment_count_ckpt == 0
            || segment_count_sit == 0
            || segment_count_nat == 0
            || segment_count_ssa == 0
            || segment_count_main == 0
            || segment0_blkaddr != cp_blkaddr
            || cp_blkaddr.checked_add(segment_count_ckpt * F2FS_BLOCKS_PER_SEGMENT)
                != Some(sit_blkaddr)
            || sit_blkaddr.checked_add(segment_count_sit * F2FS_BLOCKS_PER_SEGMENT)
                != Some(nat_blkaddr)
            || nat_blkaddr.checked_add(segment_count_nat * F2FS_BLOCKS_PER_SEGMENT)
                != Some(ssa_blkaddr)
            || ssa_blkaddr.checked_add(segment_count_ssa * F2FS_BLOCKS_PER_SEGMENT)
                != Some(main_blkaddr)
            || main_blkaddr
                .checked_add(segment_count_main * F2FS_BLOCKS_PER_SEGMENT)
                .is_none_or(|end| end > total_blocks || end > block_count)
            || segment_count
                != segment_count_ckpt
                    + segment_count_sit
                    + segment_count_nat
                    + segment_count_ssa
                    + segment_count_main
            || root_nid == 0
        {
            return Err(FsError::InvalidInput);
        }

        let mut filesystem = Self {
            device,
            total_blocks,
            block_count,
            cp_blkaddr,
            nat_blkaddr,
            main_blkaddr,
            segment_count_nat,
            segment_count_main,
            extra_attr: feature & F2FS_FEATURE_EXTRA_ATTR != 0,
            flexible_inline_xattr: feature & F2FS_FEATURE_FLEXIBLE_INLINE_XATTR != 0,
            root_nid,
            checkpoint: Checkpoint {
                version: 0,
                flags: 0,
                nat_bitmap_bytes: 0,
                nat_bitmap_offset: 0,
            },
            handles: Vec::new(),
            next_fd: 1,
        };
        filesystem.checkpoint = filesystem.read_checkpoint()?;
        if filesystem.checkpoint.flags & F2FS_CP_UMOUNT_FLAG == 0
            || filesystem.checkpoint.flags & (F2FS_CP_ERROR_FLAG | F2FS_CP_DISABLED_FLAG) != 0
        {
            return Err(FsError::NotSupported);
        }
        if filesystem.read_inode(filesystem.root_nid)?.kind()? != InodeType::Directory {
            return Err(FsError::InvalidInput);
        }
        Ok(filesystem)
    }

    fn read_bytes_from(
        device: &mut dyn BlockDevice,
        offset: u64,
        output: &mut [u8],
    ) -> Result<(), FsError> {
        if output.is_empty() {
            return Ok(());
        }
        let sector_size = device.sector_size() as u64;
        let end = offset
            .checked_add(output.len() as u64)
            .ok_or(FsError::InvalidInput)?;
        let total_bytes = device
            .total_sectors()
            .checked_mul(sector_size)
            .ok_or(FsError::InvalidInput)?;
        if end > total_bytes || sector_size < 512 || !sector_size.is_multiple_of(512) {
            return Err(FsError::UnexpectedEof);
        }
        let first_sector = offset / sector_size;
        let last_sector = (end - 1) / sector_size;
        let mut sector = first_sector;
        let mut copied = 0usize;
        while sector <= last_sector {
            let count = (last_sector - sector + 1).min(32) as u16;
            let bytes = count as usize * sector_size as usize;
            let mut scratch = vec![0u8; bytes];
            device.read_sectors(sector, count, &mut scratch)?;
            let scratch_start = sector * sector_size;
            let copy_start = offset.max(scratch_start);
            let copy_end = end.min(scratch_start + bytes as u64);
            let source = (copy_start - scratch_start) as usize;
            let length = (copy_end - copy_start) as usize;
            output[copied..copied + length].copy_from_slice(&scratch[source..source + length]);
            copied += length;
            sector = sector
                .checked_add(count as u64)
                .ok_or(FsError::InvalidInput)?;
        }
        Ok(())
    }

    fn read_bytes(&mut self, offset: u64, output: &mut [u8]) -> Result<(), FsError> {
        Self::read_bytes_from(&mut *self.device, offset, output)
    }

    fn read_block(&mut self, block: u64, output: &mut [u8]) -> Result<(), FsError> {
        if block >= self.total_blocks || block >= self.block_count || output.len() < 4096 {
            return Err(FsError::InvalidInput);
        }
        let offset = block
            .checked_mul(F2FS_BLOCK_SIZE)
            .ok_or(FsError::InvalidInput)?;
        self.read_bytes(offset, &mut output[..4096])
    }

    fn valid_node_block(&self, block: u64) -> bool {
        block >= self.main_blkaddr
            && block
                < self
                    .main_blkaddr
                    .saturating_add(self.segment_count_main * F2FS_BLOCKS_PER_SEGMENT)
    }

    fn valid_data_block(&self, block: u32) -> Result<Option<u64>, FsError> {
        if block == 0 {
            return Ok(None);
        }
        if block == u32::MAX || block == u32::MAX - 1 {
            return Err(FsError::NotSupported);
        }
        let block = block as u64;
        if !self.valid_node_block(block) {
            return Err(FsError::InvalidInput);
        }
        Ok(Some(block))
    }

    fn checkpoint_crc_valid(block: &[u8]) -> bool {
        if block.len() < F2FS_BLOCK_SIZE as usize {
            return false;
        }
        let checksum_offset = le_u32(block, 164) as usize;
        if !(F2FS_CHECKPOINT_MIN_CRC_OFFSET..=F2FS_CHECKPOINT_CRC_OFFSET).contains(&checksum_offset)
        {
            return false;
        }
        let mut crc = crc32(F2FS_SUPER_MAGIC_SEED, &block[..checksum_offset]);
        if checksum_offset < F2FS_CHECKPOINT_CRC_OFFSET {
            crc = crc32(crc, &block[checksum_offset + 4..F2FS_CHECKPOINT_CRC_OFFSET]);
        }
        crc == le_u32(block, checksum_offset)
    }

    fn read_checkpoint_pack(&mut self, start: u64) -> Result<Option<(Vec<u8>, u64)>, FsError> {
        let mut first = vec![0u8; 4096];
        self.read_block(start, &mut first)?;
        if !Self::checkpoint_crc_valid(&first) {
            return Ok(None);
        }
        let total = le_u32(&first, 136) as u64;
        if !(3..=F2FS_BLOCKS_PER_SEGMENT).contains(&total) {
            return Ok(None);
        }
        let last_block = start.checked_add(total - 1).ok_or(FsError::InvalidInput)?;
        let mut last = vec![0u8; 4096];
        self.read_block(last_block, &mut last)?;
        if !Self::checkpoint_crc_valid(&last) || le_u64(&first, 0) != le_u64(&last, 0) {
            return Ok(None);
        }
        let payload = le_u32(&first, 1664) as u64;
        if payload >= total || payload + 1 > 32 {
            return Err(FsError::InvalidInput);
        }
        let mut checkpoint = vec![0u8; (payload as usize + 1) * 4096];
        checkpoint[..4096].copy_from_slice(&first);
        for index in 1..=payload {
            self.read_block(
                start + index,
                &mut checkpoint[index as usize * 4096..][..4096],
            )?;
        }
        Ok(Some((checkpoint, le_u64(&first, 0))))
    }

    fn read_checkpoint(&mut self) -> Result<Checkpoint, FsError> {
        let first = self.read_checkpoint_pack(self.cp_blkaddr)?;
        let second_start = self
            .cp_blkaddr
            .checked_add(F2FS_BLOCKS_PER_SEGMENT)
            .ok_or(FsError::InvalidInput)?;
        let second = self.read_checkpoint_pack(second_start)?;
        let (bytes, version) = match (first, second) {
            (Some(first), Some(second)) if second.1 > first.1 => second,
            (Some(first), _) => first,
            (None, Some(second)) => second,
            (None, None) => return Err(FsError::InvalidInput),
        };
        let flags = le_u32(&bytes, 132);
        let nat_bitmap_bytes = le_u32(&bytes, 160);
        if nat_bitmap_bytes == 0 || nat_bitmap_bytes > 4096 {
            return Err(FsError::InvalidInput);
        }
        let large = flags & F2FS_CP_LARGE_NAT_BITMAP_FLAG != 0;
        let nat_bitmap_offset = if large {
            192 + 4
        } else if le_u32(&bytes, 1664) > 0 {
            192
        } else {
            192 + le_u32(&bytes, 156) as usize
        };
        if nat_bitmap_offset
            .checked_add(nat_bitmap_bytes as usize)
            .is_none_or(|end| end > bytes.len())
        {
            return Err(FsError::InvalidInput);
        }
        Ok(Checkpoint {
            version,
            flags,
            nat_bitmap_bytes,
            nat_bitmap_offset,
        })
    }

    fn nat_bitmap_bit(&mut self, block_offset: u64) -> Result<bool, FsError> {
        let checkpoint_start = if self.checkpoint.version == 0 {
            return Err(FsError::InvalidInput);
        } else {
            // The selected pack is found again by version.  This keeps the
            // filesystem state small while preserving the exact bitmap used
            // for the NAT double-buffer selection.
            let first = self.read_checkpoint_pack(self.cp_blkaddr)?;
            let second = self.read_checkpoint_pack(
                self.cp_blkaddr
                    .checked_add(F2FS_BLOCKS_PER_SEGMENT)
                    .ok_or(FsError::InvalidInput)?,
            )?;
            match (first, second) {
                (Some(_first), Some(second)) if second.1 == self.checkpoint.version => second.0,
                (Some(first), _) if first.1 == self.checkpoint.version => first.0,
                (_, Some(second)) if second.1 == self.checkpoint.version => second.0,
                _ => return Err(FsError::InvalidInput),
            }
        };
        if block_offset >= self.checkpoint.nat_bitmap_bytes as u64 * 8 {
            return Err(FsError::InvalidInput);
        }
        let byte = checkpoint_start[self.checkpoint.nat_bitmap_offset + block_offset as usize / 8];
        Ok(byte & (0x80 >> (block_offset & 7)) != 0)
    }

    fn nat_block(&mut self, nid: u32) -> Result<u64, FsError> {
        let block_offset = nid as u64 / NAT_ENTRY_PER_BLOCK;
        let segment_offset = block_offset & (F2FS_BLOCKS_PER_SEGMENT - 1);
        let mut block = self
            .nat_blkaddr
            .checked_add(block_offset * 2)
            .and_then(|block| block.checked_sub(segment_offset))
            .ok_or(FsError::InvalidInput)?;
        if self.nat_bitmap_bit(block_offset)? {
            block = block
                .checked_add(F2FS_BLOCKS_PER_SEGMENT)
                .ok_or(FsError::InvalidInput)?;
        }
        let nat_end = self
            .nat_blkaddr
            .checked_add(self.segment_count_nat * F2FS_BLOCKS_PER_SEGMENT)
            .ok_or(FsError::InvalidInput)?;
        if block < self.nat_blkaddr || block >= nat_end {
            return Err(FsError::InvalidInput);
        }
        Ok(block)
    }

    fn node_block_for_nid(&mut self, nid: u32) -> Result<u64, FsError> {
        if nid < 3 {
            return Err(FsError::InvalidInput);
        }
        let nat_block = self.nat_block(nid)?;
        let entry = (nid as u64 % NAT_ENTRY_PER_BLOCK) as usize;
        let offset = entry
            .checked_mul(NAT_ENTRY_SIZE)
            .ok_or(FsError::InvalidInput)?;
        let mut raw = vec![0u8; 4096];
        self.read_block(nat_block, &mut raw)?;
        let stored_nid = le_u32(&raw, offset + 1);
        if stored_nid != nid {
            return Err(FsError::FileNotFound);
        }
        let block = le_u32(&raw, offset + 5);
        if block == 0 || block == u32::MAX || !self.valid_node_block(block as u64) {
            return Err(FsError::FileNotFound);
        }
        Ok(block as u64)
    }

    fn read_node(&mut self, nid: u32) -> Result<Vec<u8>, FsError> {
        let block = self.node_block_for_nid(nid)?;
        let mut raw = vec![0u8; 4096];
        self.read_block(block, &mut raw)?;
        let footer = 4096 - 24;
        if le_u32(&raw, footer) != nid || le_u32(&raw, footer + 4) == 0 {
            return Err(FsError::InvalidInput);
        }
        Ok(raw)
    }

    fn read_inode(&mut self, nid: u32) -> Result<InodeRecord, FsError> {
        let node = self.read_node(nid)?;
        let inline_flags = node[3];
        if inline_flags & F2FS_EXTRA_ATTR != 0 && !self.extra_attr {
            return Err(FsError::InvalidInput);
        }
        let extra_isize = if inline_flags & F2FS_EXTRA_ATTR != 0 {
            let value = le_u16(&node, OFFSET_OF_END_OF_I_EXT) as usize;
            if value == 0 || value % 4 != 0 || value > 256 {
                return Err(FsError::InvalidInput);
            }
            value
        } else {
            0
        };
        let inline_xattr_words = if inline_flags & (F2FS_INLINE_XATTR | F2FS_INLINE_DENTRY) != 0 {
            if inline_flags & F2FS_EXTRA_ATTR != 0 && self.flexible_inline_xattr {
                let value = le_u16(&node, OFFSET_OF_END_OF_I_EXT + 2) as usize;
                if value > 200 {
                    return Err(FsError::InvalidInput);
                }
                value
            } else {
                50
            }
        } else {
            0
        };
        let address_words = DEF_ADDRS_PER_INODE
            .checked_sub(extra_isize / 4)
            .and_then(|words| words.checked_sub(inline_xattr_words))
            .ok_or(FsError::InvalidInput)?;
        let address_end = OFFSET_OF_END_OF_I_EXT
            .checked_add(extra_isize)
            .and_then(|offset| offset.checked_add(address_words * 4))
            .ok_or(FsError::InvalidInput)?;
        if address_end > 4052 || node[3] & 0x80 != 0 {
            return Err(FsError::NotSupported);
        }
        let flags = le_u32(&node, 80);
        if flags & (F2FS_COMPR_FL | F2FS_CASEFOLD_FL) != 0 {
            return Err(FsError::NotSupported);
        }
        Ok(InodeRecord {
            nid,
            mode: le_u16(&node, 0),
            uid: le_u32(&node, 4),
            gid: le_u32(&node, 8),
            inline_flags,
            size: le_u64(&node, 16),
            extra_isize,
            address_words,
            node,
        })
    }

    fn inode_address(&self, inode: &InodeRecord, index: usize) -> Result<Option<u64>, FsError> {
        if index >= inode.address_words {
            return Err(FsError::InvalidInput);
        }
        let offset = OFFSET_OF_END_OF_I_EXT
            .checked_add(inode.extra_isize)
            .and_then(|offset| offset.checked_add(index * 4))
            .ok_or(FsError::InvalidInput)?;
        self.valid_data_block(le_u32(&inode.node, offset))
    }

    fn inode_nid(&self, inode: &InodeRecord, index: usize) -> Result<Option<u32>, FsError> {
        if index >= DEF_NIDS_PER_INODE {
            return Err(FsError::InvalidInput);
        }
        let offset = 4052 + index * 4;
        let nid = le_u32(&inode.node, offset);
        Ok((nid != 0).then_some(nid))
    }

    fn node_address(
        &mut self,
        nid: u32,
        index: usize,
        expect_inode: u32,
    ) -> Result<Option<u64>, FsError> {
        let node = self.read_node(nid)?;
        let footer = 4096 - 24;
        if le_u32(&node, footer + 4) != expect_inode || index >= DEF_ADDRS_PER_BLOCK {
            return Err(FsError::InvalidInput);
        }
        self.valid_data_block(le_u32(&node, index * 4))
    }

    fn indirect_nid(
        &mut self,
        nid: u32,
        index: usize,
        expect_inode: u32,
    ) -> Result<Option<u32>, FsError> {
        let node = self.read_node(nid)?;
        let footer = 4096 - 24;
        if le_u32(&node, footer + 4) != expect_inode || index >= DEF_ADDRS_PER_BLOCK {
            return Err(FsError::InvalidInput);
        }
        let value = le_u32(&node, index * 4);
        Ok((value != 0).then_some(value))
    }

    fn file_block(&mut self, inode: &InodeRecord, logical: u64) -> Result<Option<u64>, FsError> {
        let direct = inode.address_words as u64;
        if logical < direct {
            return self.inode_address(inode, logical as usize);
        }
        let mut logical = logical - direct;
        let per_node = DEF_ADDRS_PER_BLOCK as u64;
        if logical < per_node * 2 {
            let slot = (logical / per_node) as usize;
            let index = (logical % per_node) as usize;
            return match self.inode_nid(inode, slot)? {
                Some(nid) => self.node_address(nid, index, inode.nid),
                None => Ok(None),
            };
        }
        logical -= per_node * 2;
        let single_span = per_node * per_node;
        if logical < single_span * 2 {
            let slot = 2 + (logical / single_span) as usize;
            let within = logical % single_span;
            let indirect_index = (within / per_node) as usize;
            let address_index = (within % per_node) as usize;
            let Some(indirect) = self.inode_nid(inode, slot)? else {
                return Ok(None);
            };
            let Some(direct_nid) = self.indirect_nid(indirect, indirect_index, inode.nid)? else {
                return Ok(None);
            };
            return self.node_address(direct_nid, address_index, inode.nid);
        }
        logical -= single_span * 2;
        let double_span = single_span * per_node;
        if logical >= double_span {
            return Err(FsError::NotSupported);
        }
        let Some(first_indirect) = self.inode_nid(inode, 4)? else {
            return Ok(None);
        };
        let first_index = (logical / single_span) as usize;
        let within = logical % single_span;
        let second_index = (within / per_node) as usize;
        let address_index = (within % per_node) as usize;
        let Some(second_indirect) = self.indirect_nid(first_indirect, first_index, inode.nid)?
        else {
            return Ok(None);
        };
        let Some(direct_nid) = self.indirect_nid(second_indirect, second_index, inode.nid)? else {
            return Ok(None);
        };
        self.node_address(direct_nid, address_index, inode.nid)
    }

    fn read_inode_bytes(
        &mut self,
        inode: &InodeRecord,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, FsError> {
        if offset >= inode.size || output.is_empty() {
            return Ok(0);
        }
        let length = (inode.size - offset).min(output.len() as u64) as usize;
        if inode.inline_flags & F2FS_INLINE_DATA != 0 {
            let area_offset = inode.inline_area_offset()?;
            let area_bytes = inode.inline_area_bytes()?;
            if inode.size > area_bytes as u64 || offset > area_bytes as u64 {
                return Err(FsError::InvalidInput);
            }
            let end = offset as usize + length;
            if end > area_bytes {
                return Err(FsError::InvalidInput);
            }
            output[..length]
                .copy_from_slice(&inode.node[area_offset + offset as usize..area_offset + end]);
            return Ok(length);
        }
        let mut block_data = vec![0u8; 4096];
        let mut done = 0usize;
        while done < length {
            let position = offset + done as u64;
            let logical = position >> F2FS_BLOCK_BITS;
            let within = (position & (F2FS_BLOCK_SIZE - 1)) as usize;
            let take = (4096 - within).min(length - done);
            if let Some(block) = self.file_block(inode, logical)? {
                self.read_block(block, &mut block_data)?;
                output[done..done + take].copy_from_slice(&block_data[within..within + take]);
            } else {
                output[done..done + take].fill(0);
            }
            done += take;
        }
        Ok(done)
    }

    fn read_symlink(&mut self, inode: &InodeRecord) -> Result<String, FsError> {
        if inode.size > MAX_SYMLINK_BYTES {
            return Err(FsError::NotSupported);
        }
        let mut bytes = vec![0u8; inode.size as usize];
        self.read_inode_bytes(inode, 0, &mut bytes)?;
        core::str::from_utf8(&bytes)
            .map(String::from)
            .map_err(|_| FsError::InvalidInput)
    }

    fn parse_directory_block(
        &self,
        block: &[u8],
        max_entries: usize,
        output: &mut Vec<DirectoryEntry>,
    ) -> Result<(), FsError> {
        if block.len() < 4096 {
            return Err(FsError::UnexpectedEof);
        }
        for slot in 0..max_entries {
            let bitmap = block[slot / 8];
            if bitmap & (0x80 >> (slot & 7)) == 0 {
                continue;
            }
            let entry_offset = F2FS_DENTRY_ENTRY_OFFSET + slot * F2FS_DENTRY_SIZE;
            let entry_end = entry_offset + F2FS_DENTRY_SIZE;
            if entry_end > block.len() {
                return Err(FsError::InvalidInput);
            }
            let nid = le_u32(block, entry_offset + 4);
            let name_len = le_u16(block, entry_offset + 8) as usize;
            let slots = name_len
                .checked_add(F2FS_SLOT_LEN - 1)
                .ok_or(FsError::InvalidInput)?
                / F2FS_SLOT_LEN;
            if nid == 0 || name_len == 0 || slots == 0 || slot + slots > max_entries {
                continue;
            }
            let name_offset = F2FS_DENTRY_NAME_OFFSET
                .checked_add(slot * F2FS_SLOT_LEN)
                .ok_or(FsError::InvalidInput)?;
            let name_end = name_offset
                .checked_add(slots * F2FS_SLOT_LEN)
                .ok_or(FsError::InvalidInput)?;
            if name_end > block.len() {
                return Err(FsError::InvalidInput);
            }
            let name_bytes = &block[name_offset..name_offset + name_len];
            if let Ok(name) = core::str::from_utf8(name_bytes) {
                output.push(DirectoryEntry {
                    nid,
                    name: String::from(name),
                });
                if output.len() >= MAX_DIR_ENTRIES {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn directory_entries(&mut self, inode: &InodeRecord) -> Result<Vec<DirectoryEntry>, FsError> {
        if inode.kind()? != InodeType::Directory {
            return Err(FsError::NotADirectory);
        }
        let mut result = Vec::new();
        if inode.inline_flags & F2FS_INLINE_DENTRY != 0 {
            let offset = inode.inline_area_offset()?;
            let bytes = inode.inline_area_bytes()?;
            let entry_count = (bytes * 8) / ((F2FS_DENTRY_SIZE + F2FS_SLOT_LEN) * 8 + 1);
            let bitmap_size = (entry_count + 7) / 8;
            let reserved = bytes
                .checked_sub((F2FS_DENTRY_SIZE + F2FS_SLOT_LEN) * entry_count)
                .and_then(|value| value.checked_sub(bitmap_size))
                .ok_or(FsError::InvalidInput)?;
            let entry_offset = bitmap_size + reserved;
            let name_offset = entry_offset + F2FS_DENTRY_SIZE * entry_count;
            let end = offset.checked_add(bytes).ok_or(FsError::InvalidInput)?;
            if end > inode.node.len() || entry_offset > bytes || name_offset > bytes {
                return Err(FsError::InvalidInput);
            }
            // Inline dentries have the same slot/entry encoding but a smaller
            // inline region.  Normalize it into a block-sized scratch buffer.
            let mut normalized = vec![0u8; 4096];
            normalized[..bytes].copy_from_slice(&inode.node[offset..end]);
            let _ = (entry_offset, name_offset);
            self.parse_inline_directory(
                &normalized,
                entry_count,
                bitmap_size,
                entry_offset,
                name_offset,
                &mut result,
            )?;
            return Ok(result);
        }
        let blocks = inode
            .size
            .checked_add(F2FS_BLOCK_SIZE - 1)
            .ok_or(FsError::InvalidInput)?
            / F2FS_BLOCK_SIZE;
        if blocks > MAX_DIRECTORY_BLOCKS {
            return Err(FsError::NotSupported);
        }
        let mut block_data = vec![0u8; 4096];
        for logical in 0..blocks {
            let Some(block) = self.file_block(inode, logical)? else {
                continue;
            };
            self.read_block(block, &mut block_data)?;
            self.parse_directory_block(&block_data, F2FS_DENTRY_COUNT, &mut result)?;
            if result.len() >= MAX_DIR_ENTRIES {
                break;
            }
        }
        Ok(result)
    }

    fn parse_inline_directory(
        &self,
        block: &[u8],
        entry_count: usize,
        bitmap_size: usize,
        entry_offset: usize,
        name_offset: usize,
        output: &mut Vec<DirectoryEntry>,
    ) -> Result<(), FsError> {
        if entry_count == 0 || bitmap_size > block.len() || name_offset > block.len() {
            return Err(FsError::InvalidInput);
        }
        for slot in 0..entry_count {
            if block[slot / 8] & (0x80 >> (slot & 7)) == 0 {
                continue;
            }
            let entry = entry_offset + slot * F2FS_DENTRY_SIZE;
            if entry + F2FS_DENTRY_SIZE > block.len() {
                return Err(FsError::InvalidInput);
            }
            let nid = le_u32(block, entry + 4);
            let name_len = le_u16(block, entry + 8) as usize;
            let slots = (name_len + F2FS_SLOT_LEN - 1) / F2FS_SLOT_LEN;
            if nid == 0 || name_len == 0 || slots == 0 || slot + slots > entry_count {
                continue;
            }
            let name = name_offset + slot * F2FS_SLOT_LEN;
            if name + name_len > block.len() {
                return Err(FsError::InvalidInput);
            }
            if let Ok(name) = core::str::from_utf8(&block[name..name + name_len]) {
                output.push(DirectoryEntry {
                    nid,
                    name: String::from(name),
                });
            }
        }
        Ok(())
    }

    fn find_child(&mut self, parent: u32, name: &str) -> Result<Option<DirectoryEntry>, FsError> {
        let inode = self.read_inode(parent)?;
        Ok(self
            .directory_entries(&inode)?
            .into_iter()
            .find(|entry| entry.name == name))
    }

    fn components(path: &str) -> Result<Vec<String>, FsError> {
        let components = path
            .split('/')
            .filter(|component| !component.is_empty())
            .map(String::from)
            .collect::<Vec<_>>();
        if components.len() > MAX_PATH_COMPONENTS {
            return Err(FsError::InvalidPath);
        }
        Ok(components)
    }

    fn lookup_components(&mut self, components: &[String], depth: u32) -> Result<u32, FsError> {
        if depth > MAX_SYMLINK_DEPTH {
            return Err(FsError::InvalidPath);
        }
        let mut current = self.root_nid;
        let mut resolved = Vec::<String>::new();
        let mut index = 0usize;
        while index < components.len() {
            let component = &components[index];
            if component == "." {
                index += 1;
                continue;
            }
            if component == ".." {
                if current != self.root_nid {
                    let parent = self
                        .find_child(current, "..")?
                        .ok_or(FsError::InvalidPath)?;
                    current = parent.nid;
                    resolved.pop();
                }
                index += 1;
                continue;
            }
            let child = self
                .find_child(current, component)?
                .ok_or(FsError::FileNotFound)?;
            let child_inode = self.read_inode(child.nid)?;
            if child_inode.kind()? == InodeType::Symlink {
                let target = self.read_symlink(&child_inode)?;
                let mut next = Vec::new();
                if !target.starts_with('/') {
                    next.extend(resolved.iter().cloned());
                }
                next.extend(Self::components(&target)?);
                next.extend(components[index + 1..].iter().cloned());
                return self.lookup_components(&next, depth + 1);
            }
            current = child.nid;
            resolved.push(component.clone());
            index += 1;
        }
        Ok(current)
    }

    fn lookup(&mut self, path: &str) -> Result<u32, FsError> {
        self.lookup_components(&Self::components(path)?, 0)
    }

    fn handle(&self, fd: u32) -> Result<(u32, u64), FsError> {
        self.handles
            .iter()
            .find(|handle| handle.fd == fd)
            .map(|handle| (handle.nid, handle.offset))
            .ok_or(FsError::InvalidFileDescriptor)
    }
}

impl FileSystem for F2fsFileSystem {
    fn capabilities(&self) -> FileSystemCapabilities {
        FileSystemCapabilities::new(true, false, false, false, true)
    }

    fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor> {
        let nid = self.lookup(path).ok()?;
        let fd = self.next_fd;
        self.next_fd = self.next_fd.checked_add(1)?;
        self.handles.push(F2fsHandle { fd, nid, offset: 0 });
        Some(FileDescriptor {
            fd,
            ino: nid as u64,
            offset: 0,
            flags,
        })
    }

    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        let (nid, offset) = self.handle(fd)?;
        let inode = self.read_inode(nid)?;
        if inode.kind()? != InodeType::File {
            return Err(FsError::IsADirectory);
        }
        let read = self.read_inode_bytes(&inode, offset, buf)?;
        if let Some(handle) = self.handles.iter_mut().find(|handle| handle.fd == fd) {
            handle.offset = handle
                .offset
                .checked_add(read as u64)
                .ok_or(FsError::InvalidSeek)?;
        }
        Ok(read)
    }

    fn write(&mut self, _fd: u32, _data: &[u8]) -> Result<usize, FsError> {
        Err(FsError::PermissionDenied)
    }

    fn close(&mut self, fd: u32) -> Result<(), FsError> {
        let position = self
            .handles
            .iter()
            .position(|handle| handle.fd == fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
        self.handles.remove(position);
        Ok(())
    }

    fn seek(&mut self, fd: u32, pos: u64) -> Result<(), FsError> {
        let handle = self
            .handles
            .iter_mut()
            .find(|handle| handle.fd == fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
        handle.offset = pos;
        Ok(())
    }

    fn position(&mut self, fd: u32) -> Result<u64, FsError> {
        Ok(self.handle(fd)?.1)
    }

    fn size(&mut self, fd: u32) -> Result<u64, FsError> {
        let (nid, _) = self.handle(fd)?;
        Ok(self.read_inode(nid)?.size)
    }

    fn metadata(&mut self, path: &str) -> Result<FileMetadata, FsError> {
        let nid = self.lookup(path)?;
        let inode = self.read_inode(nid)?;
        Ok(FileMetadata {
            mode: inode.mode as u32,
            uid: inode.uid,
            gid: inode.gid,
            size: inode.size,
            kind: inode.kind()?,
        })
    }

    fn metadata_at(&mut self, fd: u32) -> Result<FileMetadata, FsError> {
        let (nid, _) = self.handle(fd)?;
        let inode = self.read_inode(nid)?;
        Ok(FileMetadata {
            mode: inode.mode as u32,
            uid: inode.uid,
            gid: inode.gid,
            size: inode.size,
            kind: inode.kind()?,
        })
    }

    fn create(&mut self, _path: &str, _kind: InodeType) -> Option<u64> {
        None
    }

    fn mkdir(&mut self, _path: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }

    fn unlink(&mut self, _path: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }

    fn readdir(&mut self, path: &str) -> Result<Vec<VNode>, FsError> {
        let nid = self.lookup(path)?;
        let inode = self.read_inode(nid)?;
        let entries = self.directory_entries(&inode)?;
        Ok(entries
            .into_iter()
            .filter(|entry| entry.name != "." && entry.name != "..")
            .filter_map(|entry| {
                let child = self.read_inode(entry.nid).ok()?;
                let kind = child.kind().ok()?;
                Some(VNode {
                    name: entry.name,
                    size: child.size,
                    is_dir: kind == InodeType::Directory,
                })
            })
            .collect())
    }

    fn read_link(&mut self, path: &str) -> Result<String, FsError> {
        let path = path.trim_end_matches('/');
        let split = path.rfind('/');
        let (parent_path, name) = match split {
            Some(0) => ("/", &path[1..]),
            Some(index) => (&path[..index], &path[index + 1..]),
            None => ("", path),
        };
        if name.is_empty() {
            return Err(FsError::InvalidPath);
        }
        let parent = self.lookup(parent_path)?;
        let child = self
            .find_child(parent, name)?
            .ok_or(FsError::FileNotFound)?;
        let inode = self.read_inode(child.nid)?;
        if inode.kind()? != InodeType::Symlink {
            return Err(FsError::InvalidInput);
        }
        self.read_symlink(&inode)
    }

    fn exists(&mut self, path: &str) -> bool {
        self.lookup(path).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MemoryBlockDevice {
        data: Vec<u8>,
    }

    impl BlockDevice for MemoryBlockDevice {
        fn read_sectors(
            &mut self,
            lba: u64,
            count: u16,
            buf: &mut [u8],
        ) -> Result<(), crate::block::BlockError> {
            let start = lba as usize * 512;
            let len = count as usize * 512;
            let end = start
                .checked_add(len)
                .ok_or(crate::block::BlockError::LbaOverflow)?;
            if end > self.data.len() || buf.len() < len {
                return Err(crate::block::BlockError::LbaOverflow);
            }
            buf[..len].copy_from_slice(&self.data[start..end]);
            Ok(())
        }

        fn write_sectors(
            &mut self,
            _lba: u64,
            _count: u16,
            _buf: &[u8],
        ) -> Result<(), crate::block::BlockError> {
            Err(crate::block::BlockError::Device)
        }

        fn sector_size(&self) -> u32 {
            512
        }

        fn total_sectors(&self) -> u64 {
            (self.data.len() / 512) as u64
        }
    }

    fn put_u16(data: &mut [u8], offset: usize, value: u16) {
        data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(data: &mut [u8], offset: usize, value: u32) {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(data: &mut [u8], offset: usize, value: u64) {
        data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn checkpoint_crc(block: &mut [u8]) {
        put_u32(block, 164, F2FS_CHECKPOINT_CRC_OFFSET as u32);
        let crc = crc32(F2FS_SUPER_MAGIC_SEED, &block[..F2FS_CHECKPOINT_CRC_OFFSET]);
        put_u32(block, F2FS_CHECKPOINT_CRC_OFFSET, crc);
    }

    fn make_image() -> Vec<u8> {
        let blocks = 13 * 512;
        let mut image = vec![0u8; blocks * 4096];
        let superblock = &mut image[1024..1024 + F2FS_SUPER_BYTES];
        put_u32(superblock, 0, F2FS_MAGIC);
        put_u32(superblock, 8, 9);
        put_u32(superblock, 12, 3);
        put_u32(superblock, 16, 12);
        put_u32(superblock, 20, 9);
        put_u64(superblock, 36, blocks as u64);
        put_u32(superblock, 48, 12);
        put_u32(superblock, 52, 2);
        put_u32(superblock, 56, 1);
        put_u32(superblock, 60, 2);
        put_u32(superblock, 64, 1);
        put_u32(superblock, 68, 6);
        put_u32(superblock, 72, 512);
        put_u32(superblock, 76, 512);
        put_u32(superblock, 80, 1536);
        put_u32(superblock, 84, 2048);
        put_u32(superblock, 88, 3072);
        put_u32(superblock, 92, 3584);
        put_u32(superblock, 96, 3);
        put_u32(superblock, 100, 1);
        put_u32(superblock, 104, 2);

        // Checkpoint pack 1, with NAT bitmap bit 0 clear.
        let cp = &mut image[512 * 4096..513 * 4096];
        put_u64(cp, 0, 1);
        put_u32(cp, 132, F2FS_CP_UMOUNT_FLAG);
        put_u32(cp, 136, 3);
        put_u32(cp, 160, 64);
        checkpoint_crc(cp);
        let cp_bytes = cp.to_vec();
        image[514 * 4096..515 * 4096].copy_from_slice(&cp_bytes);
        // Pack 2 is valid but older, so pack 1 is selected.
        let cp2 = &mut image[1024 * 4096..1025 * 4096];
        put_u64(cp2, 0, 0);
        put_u32(cp2, 132, F2FS_CP_UMOUNT_FLAG);
        put_u32(cp2, 136, 3);
        put_u32(cp2, 160, 64);
        checkpoint_crc(cp2);
        let cp2_bytes = cp2.to_vec();
        image[1026 * 4096..1027 * 4096].copy_from_slice(&cp2_bytes);

        // NAT block for NIDs 3 and 4; current copy is nat_blkaddr.
        let nat = &mut image[2048 * 4096..2049 * 4096];
        put_u32(nat, 3 * NAT_ENTRY_SIZE + 1, 3);
        put_u32(nat, 3 * NAT_ENTRY_SIZE + 5, 3584);
        put_u32(nat, 4 * NAT_ENTRY_SIZE + 1, 4);
        put_u32(nat, 4 * NAT_ENTRY_SIZE + 5, 3586);

        // Root inode node and /hello inode node.
        let root = &mut image[3584 * 4096..3585 * 4096];
        put_u16(root, 0, S_IFDIR | 0o750);
        put_u32(root, 4, 1000);
        put_u32(root, 8, 1001);
        put_u64(root, 16, 4096);
        put_u32(root, 360, 3585);
        put_u32(root, 4072, 3);
        put_u32(root, 4076, 3);
        let file = &mut image[3586 * 4096..3587 * 4096];
        put_u16(file, 0, S_IFREG | 0o640);
        put_u32(file, 4, 2000);
        put_u32(file, 8, 2001);
        put_u64(file, 16, 5);
        put_u32(file, 360, 3587);
        put_u32(file, 4072, 4);
        put_u32(file, 4076, 4);

        let dir = &mut image[3585 * 4096..3586 * 4096];
        dir[0] = 0xe0;
        for (slot, (nid, name)) in [(3u32, "."), (3, ".."), (4, "hello")]
            .into_iter()
            .enumerate()
        {
            let entry = F2FS_DENTRY_ENTRY_OFFSET + slot * F2FS_DENTRY_SIZE;
            put_u32(dir, entry + 4, nid);
            put_u16(dir, entry + 8, name.len() as u16);
            let name_offset = F2FS_DENTRY_NAME_OFFSET + slot * F2FS_SLOT_LEN;
            dir[name_offset..name_offset + name.len()].copy_from_slice(name.as_bytes());
        }
        image[3587 * 4096..3587 * 4096 + 5].copy_from_slice(b"world");
        image
    }

    #[test]
    fn mounts_directory_and_reads_file() {
        let device = Box::new(MemoryBlockDevice { data: make_image() });
        let mut fs = F2fsFileSystem::new(device).unwrap();
        let entries = fs.readdir("/").unwrap();
        assert_eq!(entries[0].name, "hello");
        let file = fs.open("/hello", 0).unwrap();
        assert_eq!(
            fs.metadata("/hello").unwrap(),
            FileMetadata {
                mode: (S_IFREG | 0o640) as u32,
                uid: 2000,
                gid: 2001,
                size: 5,
                kind: InodeType::File,
            }
        );
        assert_eq!(fs.metadata_at(file.fd).unwrap().uid, 2000);
        let mut output = [0u8; 5];
        assert_eq!(fs.read(file.fd, &mut output).unwrap(), 5);
        assert_eq!(&output, b"world");
        assert_eq!(
            fs.write(file.fd, b"!").unwrap_err(),
            FsError::PermissionDenied
        );
    }
}
