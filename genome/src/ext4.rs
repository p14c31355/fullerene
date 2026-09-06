//! Bounded read-only ext4 access for Android system/vendor partitions.
//!
//! The implementation deliberately omits journal replay, allocation, xattrs,
//! encryption, case-folding, and every write operation. It is intended to
//! expose a clean ext4 volume through Genome's VFS after the block layer has
//! already established a read-only device.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::block::BlockDevice;
use crate::fs::FsError;
use crate::vfs::{FileDescriptor, FileSystem, FileSystemCapabilities, InodeType, VNode};

const SUPERBLOCK_OFFSET: u64 = 1024;
const SUPERBLOCK_BYTES: usize = 1024;
const EXT4_MAGIC: u16 = 0xef53;
const EXT4_EXTENTS_FL: u32 = 0x0008_0000;
const EXT4_EXTENT_MAGIC: u16 = 0xf30a;
const EXT4_EXTENT_UNWRITTEN: u16 = 0x8000;
const EXT4_ROOT_INODE: u32 = 2;
const EXT4_S_IFMT: u16 = 0xf000;
const EXT4_S_IFREG: u16 = 0x8000;
const EXT4_S_IFDIR: u16 = 0x4000;
const EXT4_S_IFLNK: u16 = 0xa000;
const EXT4_FEATURE_INCOMPAT_FILETYPE: u32 = 0x0002;
const EXT4_FEATURE_INCOMPAT_EXTENTS: u32 = 0x0040;
const EXT4_FEATURE_INCOMPAT_64BIT: u32 = 0x0080;
const EXT4_FEATURE_INCOMPAT_FLEX_BG: u32 = 0x0200;
const EXT4_FEATURE_INCOMPAT_EA_INODE: u32 = 0x0400;
const EXT4_FEATURE_INCOMPAT_CSUM_SEED: u32 = 0x2000;
const EXT4_FEATURE_INCOMPAT_LARGEDIR: u32 = 0x4000;
const EXT4_FEATURE_INCOMPAT_RECOVER: u32 = 0x0004;
const EXT4_FEATURE_INCOMPAT_COMPRESSION: u32 = 0x0001;
const EXT4_FEATURE_INCOMPAT_JOURNAL_DEV: u32 = 0x0008;
const EXT4_FEATURE_INCOMPAT_META_BG: u32 = 0x0010;
const EXT4_FEATURE_INCOMPAT_MMP: u32 = 0x0100;
const EXT4_FEATURE_INCOMPAT_DIRDATA: u32 = 0x1000;
const EXT4_FEATURE_INCOMPAT_INLINE_DATA: u32 = 0x8000;
const EXT4_FEATURE_INCOMPAT_ENCRYPT: u32 = 0x10000;
const EXT4_FEATURE_INCOMPAT_CASEFOLD: u32 = 0x20000;
const EXT4_SUPPORTED_INCOMPAT: u32 = EXT4_FEATURE_INCOMPAT_FILETYPE
    | EXT4_FEATURE_INCOMPAT_EXTENTS
    | EXT4_FEATURE_INCOMPAT_64BIT
    | EXT4_FEATURE_INCOMPAT_FLEX_BG
    | EXT4_FEATURE_INCOMPAT_EA_INODE
    | EXT4_FEATURE_INCOMPAT_CSUM_SEED
    | EXT4_FEATURE_INCOMPAT_LARGEDIR;
const EXT4_MAX_GROUPS: u64 = 1_048_576;
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

#[derive(Clone)]
struct InodeRecord {
    mode: u16,
    flags: u32,
    size: u64,
    block: [u8; 60],
}

impl InodeRecord {
    fn kind(&self) -> Result<InodeType, FsError> {
        match self.mode & EXT4_S_IFMT {
            EXT4_S_IFREG => Ok(InodeType::File),
            EXT4_S_IFDIR => Ok(InodeType::Directory),
            EXT4_S_IFLNK => Ok(InodeType::Symlink),
            _ => Err(FsError::NotSupported),
        }
    }
}

#[derive(Clone)]
struct Ext4Handle {
    fd: u32,
    ino: u32,
    offset: u64,
}

#[derive(Clone)]
struct DirectoryEntry {
    ino: u32,
    name: String,
}

pub struct Ext4FileSystem {
    device: Box<dyn BlockDevice>,
    block_size: u64,
    blocks_count: u64,
    inodes_count: u64,
    first_data_block: u32,
    inodes_per_group: u32,
    inode_size: u16,
    descriptor_size: u16,
    group_count: u64,
    handles: Vec<Ext4Handle>,
    next_fd: u32,
}

impl Ext4FileSystem {
    pub fn new(mut device: Box<dyn BlockDevice>) -> Result<Self, FsError> {
        let sector_size = device.sector_size();
        if sector_size < 512 || !sector_size.is_multiple_of(512) || sector_size > 65_536 {
            return Err(FsError::InvalidInput);
        }
        let mut superblock = vec![0u8; SUPERBLOCK_BYTES];
        Self::read_bytes_from(&mut *device, SUPERBLOCK_OFFSET, &mut superblock)?;
        if le_u16(&superblock, 0x38) != EXT4_MAGIC {
            return Err(FsError::InvalidInput);
        }

        let log_block_size = le_u32(&superblock, 0x18);
        if log_block_size > 6 {
            return Err(FsError::NotSupported);
        }
        let block_size = 1024u64
            .checked_shl(log_block_size)
            .ok_or(FsError::InvalidInput)?;
        if block_size < 1024 || block_size > 65_536 || block_size % sector_size as u64 != 0 {
            return Err(FsError::InvalidInput);
        }

        let feature_incompat = le_u32(&superblock, 0x60);
        let _feature_ro_compat = le_u32(&superblock, 0x64);
        let unsupported = feature_incompat & !EXT4_SUPPORTED_INCOMPAT;
        if unsupported != 0
            || feature_incompat
                & (EXT4_FEATURE_INCOMPAT_RECOVER
                    | EXT4_FEATURE_INCOMPAT_COMPRESSION
                    | EXT4_FEATURE_INCOMPAT_JOURNAL_DEV
                    | EXT4_FEATURE_INCOMPAT_META_BG
                    | EXT4_FEATURE_INCOMPAT_MMP
                    | EXT4_FEATURE_INCOMPAT_DIRDATA
                    | EXT4_FEATURE_INCOMPAT_INLINE_DATA
                    | EXT4_FEATURE_INCOMPAT_ENCRYPT
                    | EXT4_FEATURE_INCOMPAT_CASEFOLD)
                != 0
        {
            return Err(FsError::NotSupported);
        }

        let blocks_low = le_u32(&superblock, 0x04) as u64;
        let blocks_high = if feature_incompat & EXT4_FEATURE_INCOMPAT_64BIT != 0 {
            le_u32(&superblock, 0x150) as u64
        } else {
            0
        };
        let blocks_count = blocks_low | (blocks_high << 32);
        let inodes_count = le_u32(&superblock, 0x00) as u64;
        let first_data_block = le_u32(&superblock, 0x14);
        let blocks_per_group = le_u32(&superblock, 0x20);
        let inodes_per_group = le_u32(&superblock, 0x28);
        if blocks_count == 0 || inodes_count == 0 || blocks_per_group == 0 || inodes_per_group == 0
        {
            return Err(FsError::InvalidInput);
        }
        if first_data_block as u64 >= blocks_count
            || (block_size == 1024 && first_data_block != 1)
            || (block_size > 1024 && first_data_block != 0)
        {
            return Err(FsError::InvalidInput);
        }
        let data_blocks = blocks_count - first_data_block as u64;
        let group_count = data_blocks
            .checked_add(blocks_per_group as u64 - 1)
            .ok_or(FsError::InvalidInput)?
            / blocks_per_group as u64;
        let inode_groups = inodes_count
            .checked_add(inodes_per_group as u64 - 1)
            .ok_or(FsError::InvalidInput)?
            / inodes_per_group as u64;
        if group_count == 0 || group_count > EXT4_MAX_GROUPS || inode_groups != group_count {
            return Err(FsError::InvalidInput);
        }

        let revision = le_u32(&superblock, 0x4c);
        let inode_size = if revision == 0 {
            128
        } else {
            le_u16(&superblock, 0x58)
        };
        if !inode_size.is_power_of_two()
            || inode_size < 128
            || inode_size as u64 > block_size
            || (inode_size as usize) < 112
        {
            return Err(FsError::InvalidInput);
        }
        let descriptor_size = if revision == 0 {
            32
        } else {
            let value = le_u16(&superblock, 0xfe);
            if value == 0 { 32 } else { value }
        };
        if descriptor_size < 32 || descriptor_size > 64 {
            return Err(FsError::NotSupported);
        }
        if feature_incompat & EXT4_FEATURE_INCOMPAT_64BIT != 0 && descriptor_size < 64 {
            return Err(FsError::InvalidInput);
        }

        let total_bytes = device
            .total_sectors()
            .checked_mul(sector_size as u64)
            .ok_or(FsError::InvalidInput)?;
        let required_bytes = blocks_count
            .checked_mul(block_size)
            .ok_or(FsError::InvalidInput)?;
        if required_bytes > total_bytes {
            return Err(FsError::UnexpectedEof);
        }

        let mut filesystem = Self {
            device,
            block_size,
            blocks_count,
            inodes_count,
            first_data_block,
            inodes_per_group,
            inode_size,
            descriptor_size,
            group_count,
            handles: Vec::new(),
            next_fd: 1,
        };
        if filesystem.read_inode(EXT4_ROOT_INODE)?.kind()? != InodeType::Directory {
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
        if block >= self.blocks_count || output.len() < self.block_size as usize {
            return Err(FsError::InvalidInput);
        }
        let offset = block
            .checked_mul(self.block_size)
            .ok_or(FsError::InvalidInput)?;
        self.read_bytes(offset, &mut output[..self.block_size as usize])
    }

    fn group_descriptor(&mut self, group: u64) -> Result<Vec<u8>, FsError> {
        if group >= self.group_count {
            return Err(FsError::InvalidInput);
        }
        let table_block = self.first_data_block as u64 + 1;
        let table_offset = table_block
            .checked_mul(self.block_size)
            .and_then(|offset| offset.checked_add(group * self.descriptor_size as u64))
            .ok_or(FsError::InvalidInput)?;
        let filesystem_bytes = self
            .blocks_count
            .checked_mul(self.block_size)
            .ok_or(FsError::InvalidInput)?;
        let table_end = table_offset
            .checked_add(self.descriptor_size as u64)
            .ok_or(FsError::InvalidInput)?;
        if table_end > filesystem_bytes {
            return Err(FsError::UnexpectedEof);
        }
        let mut descriptor = vec![0u8; self.descriptor_size as usize];
        self.read_bytes(table_offset, &mut descriptor)?;
        Ok(descriptor)
    }

    fn read_inode(&mut self, ino: u32) -> Result<InodeRecord, FsError> {
        if ino == 0 || ino as u64 > self.inodes_count {
            return Err(FsError::FileNotFound);
        }
        let zero_based = ino as u64 - 1;
        let group = zero_based / self.inodes_per_group as u64;
        let index = zero_based % self.inodes_per_group as u64;
        let descriptor = self.group_descriptor(group)?;
        let table_low = le_u32(&descriptor, 8) as u64;
        let table_high = if self.descriptor_size >= 64 {
            le_u32(&descriptor, 40) as u64
        } else {
            0
        };
        let table_block = table_low | (table_high << 32);
        if table_block >= self.blocks_count {
            return Err(FsError::InvalidInput);
        }
        let inode_offset = table_block
            .checked_mul(self.block_size)
            .and_then(|offset| offset.checked_add(index * self.inode_size as u64))
            .ok_or(FsError::InvalidInput)?;
        let inode_end = inode_offset
            .checked_add(self.inode_size as u64)
            .ok_or(FsError::InvalidInput)?;
        let filesystem_bytes = self
            .blocks_count
            .checked_mul(self.block_size)
            .ok_or(FsError::InvalidInput)?;
        if inode_end > filesystem_bytes {
            return Err(FsError::UnexpectedEof);
        }
        let mut raw = vec![0u8; self.inode_size as usize];
        self.read_bytes(inode_offset, &mut raw)?;
        let mut block = [0u8; 60];
        block.copy_from_slice(&raw[40..100]);
        let size_low = le_u32(&raw, 4) as u64;
        let size_high = le_u32(&raw, 108) as u64;
        Ok(InodeRecord {
            mode: le_u16(&raw, 0),
            flags: le_u32(&raw, 32),
            size: size_low | (size_high << 32),
            block,
        })
    }

    fn read_pointer(&mut self, block: u64, index: u64) -> Result<u64, FsError> {
        if block >= self.blocks_count {
            return Err(FsError::InvalidInput);
        }
        let entry_size = 4u64;
        let offset = index.checked_mul(entry_size).ok_or(FsError::InvalidInput)?;
        if offset
            .checked_add(entry_size)
            .is_none_or(|end| end > self.block_size)
        {
            return Err(FsError::InvalidInput);
        }
        let base = block
            .checked_mul(self.block_size)
            .ok_or(FsError::InvalidInput)?;
        let mut bytes = [0u8; 4];
        self.read_bytes(base + offset, &mut bytes)?;
        Ok(u32::from_le_bytes(bytes) as u64)
    }

    fn legacy_block(&mut self, inode: &InodeRecord, logical: u64) -> Result<Option<u64>, FsError> {
        let pointers = self.block_size / 4;
        if logical < 12 {
            let block = le_u32(&inode.block, logical as usize * 4) as u64;
            return Ok((block != 0).then_some(block));
        }
        let mut logical = logical - 12;
        let mut level = 1u32;
        let mut span = pointers;
        while level <= 3 && logical >= span {
            logical -= span;
            level += 1;
            span = span.checked_mul(pointers).ok_or(FsError::InvalidInput)?;
        }
        if level > 3 {
            return Ok(None);
        }
        let root_index = 11 + level as usize;
        let mut block = le_u32(&inode.block, root_index * 4) as u64;
        if block == 0 {
            return Ok(None);
        }
        let mut remaining_level = level;
        while remaining_level != 0 {
            let divisor = if remaining_level == 1 {
                1
            } else {
                pointers.pow(remaining_level - 1)
            };
            let index = (logical / divisor) % pointers;
            block = self.read_pointer(block, index)?;
            if block == 0 {
                return Ok(None);
            }
            remaining_level -= 1;
        }
        Ok(Some(block))
    }

    fn extent_node(
        &mut self,
        node: &[u8],
        depth: u16,
        logical: u64,
        level: u16,
    ) -> Result<Option<u64>, FsError> {
        if node.len() < 12 || depth > 5 || level > 5 || le_u16(node, 0) != EXT4_EXTENT_MAGIC {
            return Err(FsError::InvalidInput);
        }
        let entries = le_u16(node, 2) as usize;
        let maximum = le_u16(node, 4) as usize;
        let entries_bytes = maximum.checked_mul(12).ok_or(FsError::InvalidInput)?;
        if entries > maximum
            || 12usize
                .checked_add(entries_bytes)
                .is_none_or(|end| end > node.len())
        {
            return Err(FsError::InvalidInput);
        }
        if depth == 0 {
            for index in 0..entries {
                let offset = 12 + index * 12;
                let first = le_u32(node, offset) as u64;
                let raw_length = le_u16(node, offset + 4);
                let length = (raw_length & !EXT4_EXTENT_UNWRITTEN) as u64;
                if length == 0 {
                    return Err(FsError::InvalidInput);
                }
                let end = first.checked_add(length).ok_or(FsError::InvalidInput)?;
                if logical < first || logical >= end {
                    continue;
                }
                if raw_length & EXT4_EXTENT_UNWRITTEN != 0 && raw_length != EXT4_EXTENT_UNWRITTEN {
                    return Ok(None);
                }
                let physical =
                    (le_u16(node, offset + 6) as u64) << 32 | le_u32(node, offset + 8) as u64;
                return Ok(Some(physical + logical - first));
            }
            return Ok(None);
        }

        let mut selected = None;
        for index in 0..entries {
            let offset = 12 + index * 12;
            let first = le_u32(node, offset) as u64;
            if first > logical {
                break;
            }
            selected = Some(offset);
        }
        let Some(offset) = selected else {
            return Ok(None);
        };
        let child = (le_u16(node, offset + 8) as u64) << 32 | le_u32(node, offset + 4) as u64;
        if child >= self.blocks_count {
            return Err(FsError::InvalidInput);
        }
        let mut child_data = vec![0u8; self.block_size as usize];
        self.read_block(child, &mut child_data)?;
        self.extent_node(&child_data, depth - 1, logical, level + 1)
    }

    fn file_block(&mut self, inode: &InodeRecord, logical: u64) -> Result<Option<u64>, FsError> {
        if inode.flags & EXT4_EXTENTS_FL != 0 {
            let depth = le_u16(&inode.block, 6);
            self.extent_node(&inode.block, depth, logical, 0)
        } else {
            self.legacy_block(inode, logical)
        }
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
        let block_size = self.block_size as usize;
        let mut block_data = vec![0u8; block_size];
        let mut done = 0usize;
        while done < length {
            let position = offset + done as u64;
            let logical = position / self.block_size;
            let within = position as usize % block_size;
            let take = (block_size - within).min(length - done);
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
        if inode.size <= inode.block.len() as u64 && inode.flags & EXT4_EXTENTS_FL == 0 {
            let bytes = &inode.block[..inode.size as usize];
            return core::str::from_utf8(bytes)
                .map(String::from)
                .map_err(|_| FsError::InvalidInput);
        }
        let mut bytes = vec![0u8; usize::try_from(inode.size).map_err(|_| FsError::InvalidInput)?];
        self.read_inode_bytes(inode, 0, &mut bytes)?;
        core::str::from_utf8(&bytes)
            .map(String::from)
            .map_err(|_| FsError::InvalidInput)
    }

    fn directory_entries(&mut self, inode: &InodeRecord) -> Result<Vec<DirectoryEntry>, FsError> {
        if inode.kind()? != InodeType::Directory {
            return Err(FsError::NotADirectory);
        }
        let block_size = self.block_size as usize;
        let blocks = inode
            .size
            .checked_add(self.block_size - 1)
            .ok_or(FsError::InvalidInput)?
            / self.block_size;
        if blocks > MAX_DIRECTORY_BLOCKS {
            return Err(FsError::NotSupported);
        }
        let mut result = Vec::new();
        let mut block_data = vec![0u8; block_size];
        for logical in 0..blocks {
            let Some(block) = self.file_block(inode, logical)? else {
                continue;
            };
            self.read_block(block, &mut block_data)?;
            let mut offset = 0usize;
            while offset + 8 <= block_size {
                let entry_ino = le_u32(&block_data, offset);
                let record_length = le_u16(&block_data, offset + 4) as usize;
                let name_length = block_data[offset + 6] as usize;
                let _file_type = block_data[offset + 7];
                if record_length == 0 {
                    break;
                }
                if record_length < 8
                    || record_length % 4 != 0
                    || record_length > block_size - offset
                    || name_length > record_length - 8
                {
                    break;
                }
                if entry_ino != 0 && name_length != 0 {
                    if let Ok(name) =
                        core::str::from_utf8(&block_data[offset + 8..offset + 8 + name_length])
                    {
                        result.push(DirectoryEntry {
                            ino: entry_ino,
                            name: String::from(name),
                        });
                        if result.len() == MAX_DIR_ENTRIES {
                            return Ok(result);
                        }
                    }
                }
                offset += record_length;
            }
        }
        Ok(result)
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
        let mut current = EXT4_ROOT_INODE;
        let mut resolved = Vec::<String>::new();
        let mut index = 0usize;
        while index < components.len() {
            let component = &components[index];
            if component == "." {
                index += 1;
                continue;
            }
            if component == ".." {
                if current != EXT4_ROOT_INODE {
                    let parent = self
                        .find_child(current, "..")?
                        .ok_or(FsError::InvalidPath)?;
                    current = parent.ino;
                    resolved.pop();
                }
                index += 1;
                continue;
            }
            let child = self
                .find_child(current, component)?
                .ok_or(FsError::FileNotFound)?;
            let child_inode = self.read_inode(child.ino)?;
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
            current = child.ino;
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
            .map(|handle| (handle.ino, handle.offset))
            .ok_or(FsError::InvalidFileDescriptor)
    }
}

impl FileSystem for Ext4FileSystem {
    fn capabilities(&self) -> FileSystemCapabilities {
        FileSystemCapabilities::new(true, false, false, false, true)
    }

    fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor> {
        let ino = self.lookup(path).ok()?;
        let fd = self.next_fd;
        self.next_fd = self.next_fd.checked_add(1)?;
        self.handles.push(Ext4Handle { fd, ino, offset: 0 });
        Some(FileDescriptor {
            fd,
            ino: ino as u64,
            offset: 0,
            flags,
        })
    }

    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        let (ino, offset) = self.handle(fd)?;
        let inode = self.read_inode(ino)?;
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
        let (ino, _) = self.handle(fd)?;
        Ok(self.read_inode(ino)?.size)
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
        let ino = self.lookup(path)?;
        let inode = self.read_inode(ino)?;
        let entries = self.directory_entries(&inode)?;
        Ok(entries
            .into_iter()
            .filter(|entry| entry.name != "." && entry.name != "..")
            .filter_map(|entry| {
                let child = self.read_inode(entry.ino).ok()?;
                let kind = child.kind().ok()?;
                Some(VNode {
                    name: entry.name,
                    size: child.size,
                    is_dir: kind == InodeType::Directory,
                })
            })
            .collect())
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

    fn make_image() -> Vec<u8> {
        let block_size = 1024usize;
        let mut image = vec![0u8; 64 * block_size];
        let superblock = &mut image[1024..2048];
        put_u32(superblock, 0x00, 8);
        put_u32(superblock, 0x04, 64);
        put_u32(superblock, 0x14, 1);
        put_u32(superblock, 0x18, 0);
        put_u32(superblock, 0x20, 64);
        put_u32(superblock, 0x28, 8);
        put_u16(superblock, 0x38, EXT4_MAGIC);
        put_u32(superblock, 0x4c, 1);
        put_u16(superblock, 0x58, 128);
        put_u16(superblock, 0xfe, 32);

        // For a 1 KiB filesystem the group descriptor table starts at block 2.
        put_u32(&mut image[2 * block_size..3 * block_size], 8, 3);
        // Inode 2 is the root directory and inode 3 is /hello.
        let inode_table = &mut image[3 * block_size..4 * block_size];
        let root_inode = &mut inode_table[128..256];
        put_u16(root_inode, 0, EXT4_S_IFDIR);
        put_u32(root_inode, 4, block_size as u32);
        put_u32(root_inode, 32, EXT4_EXTENTS_FL);
        put_u16(root_inode, 40, EXT4_EXTENT_MAGIC);
        put_u16(root_inode, 42, 1);
        put_u16(root_inode, 44, 4);
        put_u32(root_inode, 52, 0);
        put_u16(root_inode, 56, 1);
        put_u32(root_inode, 60, 10);

        let file_inode = &mut inode_table[256..384];
        put_u16(file_inode, 0, EXT4_S_IFREG);
        put_u32(file_inode, 4, 5);
        put_u32(file_inode, 32, EXT4_EXTENTS_FL);
        put_u16(file_inode, 40, EXT4_EXTENT_MAGIC);
        put_u16(file_inode, 42, 1);
        put_u16(file_inode, 44, 4);
        put_u32(file_inode, 52, 0);
        put_u16(file_inode, 56, 1);
        put_u32(file_inode, 60, 11);

        let dir = &mut image[10 * block_size..11 * block_size];
        put_u32(dir, 0, 2);
        put_u16(dir, 4, 12);
        dir[6] = 1;
        dir[8] = b'.';
        put_u32(dir, 12, 2);
        put_u16(dir, 16, 12);
        dir[18] = 2;
        dir[20..22].copy_from_slice(b"..");
        put_u32(dir, 24, 3);
        put_u16(dir, 28, (block_size - 24) as u16);
        dir[30] = 5;
        dir[31] = 1;
        dir[32..37].copy_from_slice(b"hello");
        image[11 * block_size..11 * block_size + 5].copy_from_slice(b"world");
        image
    }

    #[test]
    fn mounts_extents_directory_and_reads_file() {
        let device = Box::new(MemoryBlockDevice { data: make_image() });
        let mut fs = Ext4FileSystem::new(device).unwrap();
        let entries = fs.readdir("/").unwrap();
        assert_eq!(entries[0].name, "hello");
        let file = fs.open("/hello", 0).unwrap();
        let mut output = [0u8; 5];
        assert_eq!(fs.read(file.fd, &mut output).unwrap(), 5);
        assert_eq!(&output, b"world");
        assert_eq!(
            fs.write(file.fd, b"!").unwrap_err(),
            FsError::PermissionDenied
        );
    }
}
