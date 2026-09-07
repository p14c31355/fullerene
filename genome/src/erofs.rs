//! Bounded read-only access to the EROFS core on-disk format.
//!
//! Android may store immutable logical partitions as EROFS instead of ext4.
//! This module implements the uncompressed core layouts (plain and
//! tail-inline files) plus a bounded compressed-inode subset.  Compressed
//! files are accepted only when they use the full (non-compact) index format,
//! independent one-cluster pclusters, and LZ4/plain lclusters.  Chunked,
//! multi-device, fragmented, compact-index, and 48-bit extensions are
//! rejected at mount time instead of being guessed.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::block::BlockDevice;
use crate::fs::FsError;
use crate::vfs::{
    FileDescriptor, FileMetadata, FileSystem, FileSystemCapabilities, InodeType, VNode,
};

const SUPERBLOCK_OFFSET: u64 = 1024;
const SUPERBLOCK_BYTES: usize = 128;
const EROFS_MAGIC: u32 = 0xe0f5_e1e2;
const EROFS_INODE_SIZE_COMPACT: u64 = 32;
const EROFS_INODE_SIZE_EXTENDED: u64 = 64;
const EROFS_INODE_VERSION_MASK: u16 = 0x0001;
const EROFS_INODE_DATALAYOUT_MASK: u16 = 0x000e;
const EROFS_INODE_ALLOWED_BITS: u16 = 0x001f;
const EROFS_INODE_FLAT_PLAIN: u8 = 0;
const EROFS_INODE_COMPRESSED_FULL: u8 = 1;
const EROFS_INODE_FLAT_INLINE: u8 = 2;
const EROFS_INODE_COMPRESSED_COMPACT: u8 = 3;
const EROFS_LCLUSTER_TYPE_PLAIN: u16 = 0;
const EROFS_LCLUSTER_TYPE_HEAD1: u16 = 1;
const EROFS_LCLUSTER_TYPE_NONHEAD: u16 = 2;
const EROFS_LCLUSTER_TYPE_HEAD2: u16 = 3;
const EROFS_LCLUSTER_TYPE_MASK: u16 = 0x0003;
const EROFS_LI_PARTIAL_REF: u16 = 1 << 15;
const EROFS_ADVISE_EXTENTS: u16 = 0x0001;
const EROFS_ADVISE_BIG_PCLUSTER_1: u16 = 0x0002;
const EROFS_ADVISE_BIG_PCLUSTER_2: u16 = 0x0004;
const EROFS_ADVISE_INLINE_PCLUSTER: u16 = 0x0008;
const EROFS_ADVISE_INTERLACED_PCLUSTER: u16 = 0x0010;
const EROFS_ADVISE_FRAGMENT_PCLUSTER: u16 = 0x0020;
const EROFS_FEATURE_INCOMPAT_LZ4_0PADDING: u32 = 0x0000_0001;
const EROFS_FEATURE_INCOMPAT_COMPR_CFGS: u32 = 0x0000_0002;
const EROFS_FEATURE_INCOMPAT_COMPR_HEAD2: u32 = 0x0000_0008;
const EROFS_COMPRESSION_LZ4: u8 = 0;
const EROFS_S_IFMT: u16 = 0xf000;
const EROFS_S_IFREG: u16 = 0x8000;
const EROFS_S_IFDIR: u16 = 0x4000;
const EROFS_S_IFLNK: u16 = 0xa000;
const EROFS_DIRENT_SIZE: usize = 12;
const EROFS_MAX_BLOCK_SIZE: u64 = 65_536;
const EROFS_MAX_PATH_COMPONENTS: usize = 256;
const EROFS_MAX_DIRECTORY_BLOCKS: u64 = 131_072;
const EROFS_MAX_SYMLINK_BYTES: u64 = 4096;
const EROFS_MAX_SYMLINK_DEPTH: u32 = 8;

#[derive(Clone)]
struct ErofsHandle {
    fd: u32,
    nid: u64,
    offset: u64,
}

#[derive(Clone)]
struct ErofsInode {
    mode: u16,
    uid: u32,
    gid: u32,
    size: u64,
    layout: u8,
    start_block: u64,
    compressed_blocks: u64,
    compressed_index: u64,
    compressed_cluster_size: u64,
    compressed_algorithm: u8,
    compressed_start_block: u64,
    inline_offset: u64,
}

#[derive(Clone)]
struct DirectoryEntry {
    nid: u64,
    name: String,
}

/// A conservative read-only EROFS view suitable for immutable Android
/// partitions.  It never replays or writes metadata.
pub struct ErofsFileSystem {
    device: Box<dyn BlockDevice>,
    block_size: u64,
    total_blocks: u64,
    meta_block: u64,
    root_nid: u64,
    handles: Vec<ErofsHandle>,
    next_fd: u32,
}

impl ErofsFileSystem {
    pub fn new(mut device: Box<dyn BlockDevice>) -> Result<Self, FsError> {
        let sector_size = device.sector_size();
        if sector_size < 512 || !sector_size.is_multiple_of(512) || sector_size > 65_536 {
            return Err(FsError::InvalidInput);
        }

        let mut superblock = vec![0u8; SUPERBLOCK_BYTES];
        Self::read_bytes_from(&mut *device, SUPERBLOCK_OFFSET, &mut superblock)?;
        if le_u32(&superblock, 0) != EROFS_MAGIC {
            return Err(FsError::InvalidInput);
        }

        let blkszbits = superblock[0x0c];
        if !(9..=16).contains(&blkszbits) {
            return Err(FsError::NotSupported);
        }
        let block_size = 1u64
            .checked_shl(blkszbits as u32)
            .ok_or(FsError::InvalidInput)?;
        if !(512..=EROFS_MAX_BLOCK_SIZE).contains(&block_size)
            || !block_size.is_multiple_of(sector_size as u64)
        {
            return Err(FsError::InvalidInput);
        }

        // Keep the address/inode mapping and the compression subset bounded.
        // Android's EROFS superblock advertises available algorithms at 0x54;
        // bit zero is LZ4.  The LZ4 zero-padding, compression-config, and
        // HEAD2 flags are safe for this reader, while chunked, big-pcluster,
        // fragment, device-table, 48-bit, and other extensions are rejected.
        let feature_incompat = le_u32(&superblock, 0x50);
        if feature_incompat
            & !(EROFS_FEATURE_INCOMPAT_LZ4_0PADDING
                | EROFS_FEATURE_INCOMPAT_COMPR_CFGS
                | EROFS_FEATURE_INCOMPAT_COMPR_HEAD2)
            != 0
        {
            return Err(FsError::NotSupported);
        }
        let available_compression = le_u16(&superblock, 0x54);
        if available_compression & !(1 << EROFS_COMPRESSION_LZ4) != 0 {
            return Err(FsError::NotSupported);
        }
        if le_u16(&superblock, 0x56) != 0 || superblock[0x5a] != 0 {
            return Err(FsError::NotSupported);
        }

        let total_bytes = device
            .total_sectors()
            .checked_mul(sector_size as u64)
            .ok_or(FsError::InvalidInput)?;
        if total_bytes < block_size || !total_bytes.is_multiple_of(block_size) {
            return Err(FsError::UnexpectedEof);
        }
        let total_blocks = total_bytes / block_size;
        let meta_block = le_u32(&superblock, 0x28) as u64;
        let root_nid = le_u16(&superblock, 0x0e) as u64;
        if meta_block >= total_blocks || root_nid == 0 {
            return Err(FsError::InvalidInput);
        }

        let mut filesystem = Self {
            device,
            block_size,
            total_blocks,
            meta_block,
            root_nid,
            handles: Vec::new(),
            next_fd: 1,
        };
        if Self::kind(&filesystem.read_inode(root_nid)?)? != InodeType::Directory {
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
        if sector_size < 512 || !sector_size.is_multiple_of(512) {
            return Err(FsError::InvalidInput);
        }
        let end = offset
            .checked_add(output.len() as u64)
            .ok_or(FsError::InvalidInput)?;
        let total_bytes = device
            .total_sectors()
            .checked_mul(sector_size)
            .ok_or(FsError::InvalidInput)?;
        if end > total_bytes {
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
        if block >= self.total_blocks || output.len() < self.block_size as usize {
            return Err(FsError::InvalidInput);
        }
        let offset = block
            .checked_mul(self.block_size)
            .ok_or(FsError::InvalidInput)?;
        self.read_bytes(offset, &mut output[..self.block_size as usize])
    }

    fn inode_offset(&self, nid: u64) -> Result<u64, FsError> {
        self.meta_block
            .checked_mul(self.block_size)
            .and_then(|offset| offset.checked_add(nid.checked_mul(32)?))
            .ok_or(FsError::InvalidInput)
    }

    fn read_inode(&mut self, nid: u64) -> Result<ErofsInode, FsError> {
        if nid == 0 {
            return Err(FsError::FileNotFound);
        }
        let offset = self.inode_offset(nid)?;
        let mut compact = [0u8; EROFS_INODE_SIZE_COMPACT as usize];
        self.read_bytes(offset, &mut compact)?;
        let format = le_u16(&compact, 0);
        if format & !EROFS_INODE_ALLOWED_BITS != 0 {
            return Err(FsError::NotSupported);
        }
        let layout = ((format & EROFS_INODE_DATALAYOUT_MASK) >> 1) as u8;
        if !matches!(
            layout,
            EROFS_INODE_FLAT_PLAIN
                | EROFS_INODE_COMPRESSED_FULL
                | EROFS_INODE_FLAT_INLINE
                | EROFS_INODE_COMPRESSED_COMPACT
        ) {
            return Err(FsError::NotSupported);
        }
        if layout == EROFS_INODE_COMPRESSED_COMPACT {
            return Err(FsError::NotSupported);
        }
        let inode_size = if format & EROFS_INODE_VERSION_MASK != 0 {
            EROFS_INODE_SIZE_EXTENDED
        } else {
            EROFS_INODE_SIZE_COMPACT
        };
        let mut raw = vec![0u8; inode_size as usize];
        raw[..compact.len()].copy_from_slice(&compact);
        if inode_size > EROFS_INODE_SIZE_COMPACT {
            self.read_bytes(offset + compact.len() as u64, &mut raw[compact.len()..])?;
        }

        let xattr_count = le_u16(&raw, 2) as u64;
        let xattr_size = if xattr_count == 0 {
            0
        } else {
            12u64
                .checked_add(
                    xattr_count
                        .checked_sub(1)
                        .and_then(|count| count.checked_mul(4))
                        .ok_or(FsError::InvalidInput)?,
                )
                .ok_or(FsError::InvalidInput)?
        };
        let inline_offset = align4(
            offset
                .checked_add(inode_size)
                .and_then(|value| value.checked_add(xattr_size))
                .ok_or(FsError::InvalidInput)?,
        )?;
        let mode = le_u16(&raw, 4);
        let size = if inode_size == EROFS_INODE_SIZE_EXTENDED {
            le_u64(&raw, 8)
        } else {
            le_u32(&raw, 8) as u64
        };
        let uid = if inode_size == EROFS_INODE_SIZE_EXTENDED {
            le_u32(&raw, 24)
        } else {
            le_u16(&raw, 24) as u32
        };
        let gid = if inode_size == EROFS_INODE_SIZE_EXTENDED {
            le_u32(&raw, 28)
        } else {
            le_u16(&raw, 26) as u32
        };
        let start_block = le_u32(&raw, 16) as u64;
        let (
            compressed_blocks,
            compressed_index,
            compressed_cluster_size,
            compressed_algorithm,
            compressed_start_block,
        ) = if layout == EROFS_INODE_COMPRESSED_FULL {
            let header_offset = align8(
                offset
                    .checked_add(inode_size)
                    .and_then(|value| value.checked_add(xattr_size))
                    .ok_or(FsError::InvalidInput)?,
            )?;
            let mut header = [0u8; 8];
            self.read_bytes(header_offset, &mut header)?;
            let advise = le_u16(&header, 4);
            if advise
                & (EROFS_ADVISE_EXTENTS
                    | EROFS_ADVISE_BIG_PCLUSTER_1
                    | EROFS_ADVISE_BIG_PCLUSTER_2
                    | EROFS_ADVISE_INLINE_PCLUSTER
                    | EROFS_ADVISE_INTERLACED_PCLUSTER
                    | EROFS_ADVISE_FRAGMENT_PCLUSTER)
                != 0
                || header[7] & 0x70 != 0
                || header[7] & 0x80 != 0
            {
                return Err(FsError::NotSupported);
            }
            if advise & EROFS_ADVISE_EXTENTS != 0 || header[6] != EROFS_COMPRESSION_LZ4 {
                return Err(FsError::NotSupported);
            }
            let algorithm = EROFS_COMPRESSION_LZ4;
            let cluster_size = self
                .block_size
                .checked_shl((header[7] & 0x0f) as u32)
                .ok_or(FsError::InvalidInput)?;
            if cluster_size > 65_536 || !cluster_size.is_multiple_of(self.block_size) {
                return Err(FsError::NotSupported);
            }
            let cluster_count = size
                .checked_add(cluster_size - 1)
                .ok_or(FsError::InvalidInput)?
                / cluster_size;
            let index_offset = header_offset.checked_add(8).ok_or(FsError::InvalidInput)?;
            let index_end = index_offset
                .checked_add(cluster_count.checked_mul(8).ok_or(FsError::InvalidInput)?)
                .ok_or(FsError::InvalidInput)?;
            let total_bytes = self
                .total_blocks
                .checked_mul(self.block_size)
                .ok_or(FsError::InvalidInput)?;
            if index_end > total_bytes {
                return Err(FsError::UnexpectedEof);
            }
            let compressed_blocks = le_u32(&raw, 16) as u64;
            let compressed_start_block = if cluster_count == 0 {
                0
            } else {
                let mut index = [0u8; 8];
                self.read_bytes(index_offset, &mut index)?;
                let index_type = le_u16(&index, 0) & EROFS_LCLUSTER_TYPE_MASK;
                if index_type == EROFS_LCLUSTER_TYPE_NONHEAD
                    || le_u16(&index, 0) & EROFS_LI_PARTIAL_REF != 0
                {
                    return Err(FsError::NotSupported);
                }
                let start = le_u32(&index, 4) as u64;
                if start == 0 {
                    return Err(FsError::InvalidInput);
                }
                start
            };
            if cluster_count != 0 {
                if compressed_blocks == 0
                    || compressed_start_block
                        .checked_add(compressed_blocks)
                        .is_none_or(|end| end > self.total_blocks)
                {
                    return Err(FsError::InvalidInput);
                }
            }
            (
                compressed_blocks,
                index_offset,
                cluster_size,
                algorithm,
                compressed_start_block,
            )
        } else {
            (0, 0, 0, 0, 0)
        };
        let inode = ErofsInode {
            mode,
            uid,
            gid,
            size,
            layout,
            start_block,
            compressed_blocks,
            compressed_index,
            compressed_cluster_size,
            compressed_algorithm,
            compressed_start_block,
            inline_offset,
        };

        let inode_end = if layout == EROFS_INODE_COMPRESSED_FULL {
            compressed_index
                .checked_add(
                    size.checked_add(compressed_cluster_size - 1)
                        .ok_or(FsError::InvalidInput)?
                        / compressed_cluster_size
                        * 8,
                )
                .ok_or(FsError::InvalidInput)?
        } else {
            inline_offset
                .checked_add(if layout == EROFS_INODE_FLAT_INLINE {
                    size.min(self.block_size)
                } else {
                    0
                })
                .ok_or(FsError::InvalidInput)?
        };
        let total_bytes = self
            .total_blocks
            .checked_mul(self.block_size)
            .ok_or(FsError::InvalidInput)?;
        if inode_end > total_bytes {
            return Err(FsError::UnexpectedEof);
        }
        let physical_blocks = if layout == EROFS_INODE_FLAT_INLINE {
            size / self.block_size
        } else if layout == EROFS_INODE_COMPRESSED_FULL {
            0
        } else {
            size.checked_add(self.block_size - 1)
                .ok_or(FsError::InvalidInput)?
                / self.block_size
        };
        if physical_blocks != 0 {
            if start_block == 0
                || start_block
                    .checked_add(physical_blocks)
                    .is_none_or(|end| end > self.total_blocks)
            {
                return Err(FsError::InvalidInput);
            }
        }
        if layout == EROFS_INODE_FLAT_INLINE {
            let tail = size % self.block_size;
            if tail != 0 && inline_offset % self.block_size + tail > self.block_size {
                return Err(FsError::InvalidInput);
            }
        }
        Ok(inode)
    }

    fn kind(inode: &ErofsInode) -> Result<InodeType, FsError> {
        match inode.mode & EROFS_S_IFMT {
            EROFS_S_IFREG => Ok(InodeType::File),
            EROFS_S_IFDIR => Ok(InodeType::Directory),
            EROFS_S_IFLNK => Ok(InodeType::Symlink),
            _ => Err(FsError::NotSupported),
        }
    }

    fn file_block(&self, inode: &ErofsInode, logical: u64) -> Result<u64, FsError> {
        let block = inode
            .start_block
            .checked_add(logical)
            .ok_or(FsError::InvalidInput)?;
        if block >= self.total_blocks {
            return Err(FsError::UnexpectedEof);
        }
        Ok(block)
    }

    fn compressed_index(
        &mut self,
        inode: &ErofsInode,
        cluster: u64,
    ) -> Result<(u16, u16, u32), FsError> {
        let cluster_count = inode
            .size
            .checked_add(inode.compressed_cluster_size - 1)
            .ok_or(FsError::InvalidInput)?
            / inode.compressed_cluster_size;
        if cluster >= cluster_count {
            return Err(FsError::UnexpectedEof);
        }
        let offset = inode
            .compressed_index
            .checked_add(cluster.checked_mul(8).ok_or(FsError::InvalidInput)?)
            .ok_or(FsError::InvalidInput)?;
        let mut index = [0u8; 8];
        self.read_bytes(offset, &mut index)?;
        Ok((le_u16(&index, 0), le_u16(&index, 2), le_u32(&index, 4)))
    }

    fn compressed_block_count(
        &mut self,
        inode: &ErofsInode,
        cluster: u64,
        current_block: u64,
    ) -> Result<u64, FsError> {
        let cluster_count = inode
            .size
            .checked_add(inode.compressed_cluster_size - 1)
            .ok_or(FsError::InvalidInput)?
            / inode.compressed_cluster_size;
        let end_block = if cluster + 1 < cluster_count {
            let (next_advise, _, next_block) = self.compressed_index(inode, cluster + 1)?;
            if next_advise & EROFS_LCLUSTER_TYPE_MASK == EROFS_LCLUSTER_TYPE_NONHEAD
                || next_advise & EROFS_LI_PARTIAL_REF != 0
            {
                return Err(FsError::NotSupported);
            }
            next_block as u64
        } else {
            inode
                .compressed_start_block
                .checked_add(inode.compressed_blocks)
                .ok_or(FsError::InvalidInput)?
        };
        if end_block <= current_block || end_block > self.total_blocks {
            return Err(FsError::InvalidInput);
        }
        Ok(end_block - current_block)
    }

    fn read_compressed_cluster(
        &mut self,
        inode: &ErofsInode,
        cluster: u64,
        output: &mut [u8],
    ) -> Result<(), FsError> {
        let (advise, cluster_offset, block_address) = self.compressed_index(inode, cluster)?;
        let cluster_type = advise & EROFS_LCLUSTER_TYPE_MASK;
        if cluster_offset != 0 || advise & EROFS_LI_PARTIAL_REF != 0 {
            return Err(FsError::NotSupported);
        }
        let current_block = block_address as u64;
        let block_count = self.compressed_block_count(inode, cluster, current_block)?;
        let compressed_length = block_count
            .checked_mul(self.block_size)
            .ok_or(FsError::InvalidInput)? as usize;
        let byte_offset = current_block
            .checked_mul(self.block_size)
            .ok_or(FsError::InvalidInput)?;

        match cluster_type {
            EROFS_LCLUSTER_TYPE_PLAIN => {
                let expected = output.len() as u64;
                let available = block_count
                    .checked_mul(self.block_size)
                    .ok_or(FsError::InvalidInput)?;
                if expected > available {
                    return Err(FsError::UnexpectedEof);
                }
                self.read_bytes(byte_offset, output)
            }
            EROFS_LCLUSTER_TYPE_HEAD1 | EROFS_LCLUSTER_TYPE_HEAD2 => {
                if inode.compressed_algorithm != EROFS_COMPRESSION_LZ4 {
                    return Err(FsError::NotSupported);
                }
                let mut compressed = vec![0u8; compressed_length];
                self.read_bytes(byte_offset, &mut compressed)?;
                let decoded = lz4_decompress(&compressed, output)?;
                if decoded != output.len() {
                    return Err(FsError::UnexpectedEof);
                }
                Ok(())
            }
            EROFS_LCLUSTER_TYPE_NONHEAD => Err(FsError::NotSupported),
            _ => Err(FsError::InvalidInput),
        }
    }

    fn read_compressed_inode_bytes(
        &mut self,
        inode: &ErofsInode,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, FsError> {
        if offset >= inode.size || output.is_empty() {
            return Ok(0);
        }
        let length = (inode.size - offset).min(output.len() as u64) as usize;
        let cluster_size = inode.compressed_cluster_size as usize;
        let mut done = 0usize;
        while done < length {
            let position = offset + done as u64;
            let cluster = position / inode.compressed_cluster_size;
            let within = position as usize % cluster_size;
            let take = (cluster_size - within).min(length - done);
            let cluster_start = cluster
                .checked_mul(inode.compressed_cluster_size)
                .ok_or(FsError::InvalidInput)?;
            let cluster_length =
                (inode.size - cluster_start).min(inode.compressed_cluster_size) as usize;
            let mut decoded = vec![0u8; cluster_length];
            self.read_compressed_cluster(inode, cluster, &mut decoded)?;
            output[done..done + take].copy_from_slice(&decoded[within..within + take]);
            done += take;
        }
        Ok(length)
    }

    fn read_inode_bytes(
        &mut self,
        inode: &ErofsInode,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, FsError> {
        if offset >= inode.size || output.is_empty() {
            return Ok(0);
        }
        if inode.layout == EROFS_INODE_COMPRESSED_FULL {
            return self.read_compressed_inode_bytes(inode, offset, output);
        }
        let length = (inode.size - offset).min(output.len() as u64) as usize;
        let block_size = self.block_size as usize;
        let full_blocks = inode.size / self.block_size;
        let mut block_data = vec![0u8; block_size];
        let mut done = 0usize;
        while done < length {
            let position = offset + done as u64;
            let logical = position / self.block_size;
            let within = position as usize % block_size;
            let take = (block_size - within).min(length - done);
            if inode.layout == EROFS_INODE_FLAT_INLINE && logical >= full_blocks {
                let inline = inode
                    .inline_offset
                    .checked_add(position - full_blocks * self.block_size)
                    .ok_or(FsError::InvalidInput)?;
                self.read_bytes(inline, &mut output[done..done + take])?;
            } else {
                let block = self.file_block(inode, logical)?;
                self.read_block(block, &mut block_data)?;
                output[done..done + take].copy_from_slice(&block_data[within..within + take]);
            }
            done += take;
        }
        Ok(length)
    }

    fn components(path: &str) -> Result<Vec<String>, FsError> {
        if path.len() > 4096 {
            return Err(FsError::InvalidPath);
        }
        let mut components = Vec::new();
        for component in path.split('/').filter(|component| !component.is_empty()) {
            if component == "." || component == ".." {
                components.push(String::from(component));
                continue;
            }
            if component.len() > 255 || component.as_bytes().contains(&0) {
                return Err(FsError::InvalidPath);
            }
            components.push(String::from(component));
            if components.len() > EROFS_MAX_PATH_COMPONENTS {
                return Err(FsError::InvalidPath);
            }
        }
        Ok(components)
    }

    fn directory_entries(&mut self, inode: &ErofsInode) -> Result<Vec<DirectoryEntry>, FsError> {
        if Self::kind(inode)? != InodeType::Directory {
            return Err(FsError::NotADirectory);
        }
        let blocks = inode
            .size
            .checked_add(self.block_size - 1)
            .ok_or(FsError::InvalidInput)?
            / self.block_size;
        if blocks > EROFS_MAX_DIRECTORY_BLOCKS {
            return Err(FsError::NotSupported);
        }
        let mut entries = Vec::new();
        for block_index in 0..blocks {
            let offset = block_index * self.block_size;
            let block_len = (inode.size - offset).min(self.block_size) as usize;
            if block_len < EROFS_DIRENT_SIZE {
                continue;
            }
            let mut block = vec![0u8; self.block_size as usize];
            self.read_inode_bytes(inode, offset, &mut block[..block_len])?;
            let first_nameoff = le_u16(&block, 8) as usize;
            if first_nameoff == 0 || first_nameoff % EROFS_DIRENT_SIZE != 0 {
                return Err(FsError::InvalidInput);
            }
            let count = first_nameoff / EROFS_DIRENT_SIZE;
            if count == 0 || count > block_len / EROFS_DIRENT_SIZE {
                return Err(FsError::InvalidInput);
            }
            let table_end = count
                .checked_mul(EROFS_DIRENT_SIZE)
                .ok_or(FsError::InvalidInput)?;
            if table_end > block_len {
                return Err(FsError::InvalidInput);
            }
            for index in 0..count {
                let entry_offset = index * EROFS_DIRENT_SIZE;
                let nid = le_u64(&block, entry_offset);
                if nid == 0 || nid >> 63 != 0 {
                    return Err(FsError::InvalidInput);
                }
                let name_start = le_u16(&block, entry_offset + 8) as usize;
                let name_end = if index + 1 < count {
                    le_u16(&block, entry_offset + EROFS_DIRENT_SIZE + 8) as usize
                } else {
                    block_len
                };
                if name_start < table_end || name_start >= name_end || name_end > block_len {
                    return Err(FsError::InvalidInput);
                }
                let mut end = name_end;
                if index + 1 == count {
                    end = block[name_start..name_end]
                        .iter()
                        .position(|&byte| byte == 0)
                        .map_or(name_end, |position| name_start + position);
                }
                if end <= name_start {
                    return Err(FsError::InvalidInput);
                }
                let name = core::str::from_utf8(&block[name_start..end])
                    .map_err(|_| FsError::InvalidInput)?;
                entries.push(DirectoryEntry {
                    nid,
                    name: String::from(name),
                });
            }
        }
        Ok(entries)
    }

    fn find_child(
        &mut self,
        directory: u64,
        name: &str,
    ) -> Result<Option<DirectoryEntry>, FsError> {
        let inode = self.read_inode(directory)?;
        Ok(self
            .directory_entries(&inode)?
            .into_iter()
            .find(|entry| entry.name == name))
    }

    fn read_symlink(&mut self, inode: &ErofsInode) -> Result<String, FsError> {
        if inode.size == 0 || inode.size > EROFS_MAX_SYMLINK_BYTES {
            return Err(FsError::NotSupported);
        }
        let mut bytes = vec![0u8; inode.size as usize];
        self.read_inode_bytes(inode, 0, &mut bytes)?;
        let target = core::str::from_utf8(&bytes).map_err(|_| FsError::InvalidInput)?;
        Ok(String::from(target))
    }

    fn lookup_components(&mut self, components: &[String], depth: u32) -> Result<u64, FsError> {
        if depth > EROFS_MAX_SYMLINK_DEPTH {
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
                    current = self
                        .find_child(current, "..")?
                        .ok_or(FsError::InvalidPath)?
                        .nid;
                    resolved.pop();
                }
                index += 1;
                continue;
            }
            let child = self
                .find_child(current, component)?
                .ok_or(FsError::FileNotFound)?;
            let child_inode = self.read_inode(child.nid)?;
            if Self::kind(&child_inode)? == InodeType::Symlink {
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

    fn lookup(&mut self, path: &str) -> Result<u64, FsError> {
        self.lookup_components(&Self::components(path)?, 0)
    }

    fn handle(&self, fd: u32) -> Result<(u64, u64), FsError> {
        self.handles
            .iter()
            .find(|handle| handle.fd == fd)
            .map(|handle| (handle.nid, handle.offset))
            .ok_or(FsError::InvalidFileDescriptor)
    }
}

impl FileSystem for ErofsFileSystem {
    fn capabilities(&self) -> FileSystemCapabilities {
        FileSystemCapabilities::new(true, false, false, false, true)
    }

    fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor> {
        let nid = self.lookup(path).ok()?;
        let fd = self.next_fd;
        self.next_fd = self.next_fd.checked_add(1)?;
        self.handles.push(ErofsHandle { fd, nid, offset: 0 });
        Some(FileDescriptor {
            fd,
            ino: nid,
            offset: 0,
            flags,
        })
    }

    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        let (nid, offset) = self.handle(fd)?;
        let inode = self.read_inode(nid)?;
        if Self::kind(&inode)? != InodeType::File {
            return Err(FsError::IsADirectory);
        }
        let read = self.read_inode_bytes(&inode, offset, buf)?;
        let handle = self
            .handles
            .iter_mut()
            .find(|handle| handle.fd == fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
        handle.offset = handle
            .offset
            .checked_add(read as u64)
            .ok_or(FsError::InvalidSeek)?;
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
            kind: Self::kind(&inode)?,
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
            kind: Self::kind(&inode)?,
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
        Ok(self
            .directory_entries(&inode)?
            .into_iter()
            .filter(|entry| entry.name != "." && entry.name != "..")
            .filter_map(|entry| {
                let child = self.read_inode(entry.nid).ok()?;
                let kind = Self::kind(&child).ok()?;
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
        if Self::kind(&inode)? != InodeType::Symlink {
            return Err(FsError::InvalidInput);
        }
        self.read_symlink(&inode)
    }

    fn exists(&mut self, path: &str) -> bool {
        self.lookup(path).is_ok()
    }
}

fn align4(value: u64) -> Result<u64, FsError> {
    value
        .checked_add(3)
        .map(|value| value & !3)
        .ok_or(FsError::InvalidInput)
}

fn align8(value: u64) -> Result<u64, FsError> {
    value
        .checked_add(7)
        .map(|value| value & !7)
        .ok_or(FsError::InvalidInput)
}

/// Decode one EROFS LZ4 physical cluster into a bounded output buffer.
///
/// EROFS stores compressed data in block-sized physical clusters.  The input
/// therefore may contain zero padding after the LZ4 end-of-stream sequence;
/// decoding stops as soon as the requested logical-cluster output is full.
fn lz4_decompress(input: &[u8], output: &mut [u8]) -> Result<usize, FsError> {
    let mut input_offset = 0usize;
    let mut output_offset = 0usize;
    while output_offset < output.len() {
        let token = *input.get(input_offset).ok_or(FsError::UnexpectedEof)?;
        input_offset += 1;
        let mut literal_length = (token >> 4) as usize;
        if literal_length == 15 {
            loop {
                let extension = *input.get(input_offset).ok_or(FsError::UnexpectedEof)?;
                input_offset += 1;
                literal_length = literal_length
                    .checked_add(extension as usize)
                    .ok_or(FsError::InvalidInput)?;
                if extension != 255 {
                    break;
                }
            }
        }
        let literal_end = input_offset
            .checked_add(literal_length)
            .ok_or(FsError::InvalidInput)?;
        let output_end = output_offset
            .checked_add(literal_length)
            .ok_or(FsError::InvalidInput)?;
        if literal_end > input.len() || output_end > output.len() {
            return Err(FsError::UnexpectedEof);
        }
        output[output_offset..output_end].copy_from_slice(&input[input_offset..literal_end]);
        input_offset = literal_end;
        output_offset = output_end;
        if output_offset == output.len() {
            break;
        }

        let offset_end = input_offset.checked_add(2).ok_or(FsError::InvalidInput)?;
        if offset_end > input.len() {
            return Err(FsError::UnexpectedEof);
        }
        let match_offset =
            u16::from_le_bytes([input[input_offset], input[input_offset + 1]]) as usize;
        input_offset = offset_end;
        if match_offset == 0 || match_offset > output_offset {
            return Err(FsError::InvalidInput);
        }
        let mut match_length = (token & 0x0f) as usize + 4;
        if (token & 0x0f) == 15 {
            loop {
                let extension = *input.get(input_offset).ok_or(FsError::UnexpectedEof)?;
                input_offset += 1;
                match_length = match_length
                    .checked_add(extension as usize)
                    .ok_or(FsError::InvalidInput)?;
                if extension != 255 {
                    break;
                }
            }
        }
        let match_end = output_offset
            .checked_add(match_length)
            .ok_or(FsError::InvalidInput)?;
        if match_end > output.len() {
            return Err(FsError::UnexpectedEof);
        }
        for index in 0..match_length {
            output[output_offset + index] = output[output_offset + index - match_offset];
        }
        output_offset = match_end;
    }
    Ok(output_offset)
}

fn le_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn le_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn le_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
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

    fn make_image() -> Vec<u8> {
        let block_size = 1024usize;
        let mut image = vec![0u8; 16 * block_size];
        let superblock = &mut image[1024..1152];
        put_u32(superblock, 0, EROFS_MAGIC);
        superblock[0x0c] = 10;
        put_u16(superblock, 0x0e, 1);
        put_u32(superblock, 0x24, 16);
        put_u32(superblock, 0x28, 2);

        // Metadata block 2: NID 1 is root, NID 2 is /hello.
        let root = &mut image[2 * block_size + 32..2 * block_size + 64];
        put_u16(root, 4, EROFS_S_IFDIR | 0o750);
        put_u16(root, 24, 1000);
        put_u16(root, 26, 1001);
        put_u32(root, 8, 44);
        put_u32(root, 16, 4);

        let file = &mut image[2 * block_size + 64..2 * block_size + 96];
        put_u16(file, 4, EROFS_S_IFREG | 0o640);
        put_u16(file, 24, 2000);
        put_u16(file, 26, 2001);
        put_u32(file, 8, 5);
        put_u32(file, 16, 5);

        // EROFS directory entries: 12-byte records followed by names.
        let directory = &mut image[4 * block_size..5 * block_size];
        put_u64(directory, 0, 1);
        put_u16(directory, 8, 36);
        directory[10] = 2;
        put_u64(directory, 12, 1);
        put_u16(directory, 20, 37);
        directory[22] = 2;
        put_u64(directory, 24, 2);
        put_u16(directory, 32, 39);
        directory[34] = 1;
        directory[36] = b'.';
        directory[37..39].copy_from_slice(b"..");
        directory[39..44].copy_from_slice(b"hello");
        image[5 * block_size..5 * block_size + 5].copy_from_slice(b"world");
        image
    }

    #[test]
    fn mounts_core_plain_directory_and_reads_file() {
        let device = Box::new(MemoryBlockDevice { data: make_image() });
        let mut fs = ErofsFileSystem::new(device).unwrap();
        let entries = fs.readdir("/").unwrap();
        assert_eq!(entries[0].name, "hello");
        let file = fs.open("/hello", 0).unwrap();
        assert_eq!(
            fs.metadata("/hello").unwrap(),
            FileMetadata {
                mode: (EROFS_S_IFREG | 0o640) as u32,
                uid: 2000,
                gid: 2001,
                size: 5,
                kind: InodeType::File,
            }
        );
        assert_eq!(fs.metadata_at(file.fd).unwrap().gid, 2001);
        let mut output = [0u8; 5];
        assert_eq!(fs.read(file.fd, &mut output).unwrap(), 5);
        assert_eq!(&output, b"world");
        assert_eq!(
            fs.write(file.fd, b"!").unwrap_err(),
            FsError::PermissionDenied
        );
    }

    #[test]
    fn reads_tail_inline_file_data() {
        let mut image = make_image();
        let file = &mut image[2 * 1024 + 64..2 * 1024 + 96];
        put_u16(file, 0, EROFS_INODE_FLAT_INLINE as u16 * 2);
        put_u32(file, 8, 5);
        image[2 * 1024 + 96..2 * 1024 + 101].copy_from_slice(b"world");

        let device = Box::new(MemoryBlockDevice { data: image });
        let mut fs = ErofsFileSystem::new(device).unwrap();
        let descriptor = fs.open("/hello", 0).unwrap();
        let mut output = [0u8; 5];
        assert_eq!(fs.read(descriptor.fd, &mut output).unwrap(), 5);
        assert_eq!(&output, b"world");
    }

    #[test]
    fn reads_full_index_lz4_compressed_file() {
        let mut image = make_image();
        put_u32(
            &mut image[1024..1152],
            0x50,
            EROFS_FEATURE_INCOMPAT_LZ4_0PADDING | EROFS_FEATURE_INCOMPAT_COMPR_CFGS,
        );
        put_u16(&mut image[1024..1152], 0x54, 1 << EROFS_COMPRESSION_LZ4);
        let file = &mut image[2 * 1024 + 64..2 * 1024 + 96];
        put_u16(file, 0, EROFS_INODE_COMPRESSED_FULL as u16 * 2);
        put_u32(file, 8, 5);
        put_u32(file, 16, 1);

        // Full-index compressed inode: map header followed by one HEAD1
        // logical cluster pointing at physical block five.  The LZ4 stream
        // is a literal-only block and the rest of the physical block is
        // padding, as it is on a block-addressed EROFS volume.
        let header = &mut image[2 * 1024 + 96..2 * 1024 + 104];
        header[6] = EROFS_COMPRESSION_LZ4;
        header[7] = 0;
        let index = &mut image[2 * 1024 + 104..2 * 1024 + 112];
        put_u16(index, 0, EROFS_LCLUSTER_TYPE_HEAD1);
        put_u16(index, 2, 0);
        put_u32(index, 4, 5);
        image[5 * 1024..5 * 1024 + 6].copy_from_slice(&[0x50, b'w', b'o', b'r', b'l', b'd']);

        let device = Box::new(MemoryBlockDevice { data: image });
        let mut fs = ErofsFileSystem::new(device).unwrap();
        let descriptor = fs.open("/hello", 0).unwrap();
        let mut output = [0u8; 5];
        assert_eq!(fs.read(descriptor.fd, &mut output).unwrap(), 5);
        assert_eq!(&output, b"world");
    }
}
