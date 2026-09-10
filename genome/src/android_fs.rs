//! Android logical-partition filesystem detection and mounting.

use alloc::boxed::Box;
use alloc::vec;

use crate::block::BlockDevice;
use crate::erofs::ErofsFileSystem;
use crate::ext4::Ext4FileSystem;
use crate::f2fs::F2fsFileSystem;
use crate::fs::FsError;
use crate::vfs::FileSystem;

const PROBE_OFFSET: u64 = 1024;
const PROBE_BYTES: usize = 1024;
const EROFS_MAGIC: [u8; 4] = [0xe2, 0xe1, 0xf5, 0xe0];
const F2FS_MAGIC: [u8; 4] = [0x10, 0x20, 0xf5, 0xf2];
const EXT4_MAGIC_OFFSET: usize = 0x38;
const EXT4_MAGIC: [u8; 2] = [0x53, 0xef];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AndroidFilesystemKind {
    Ext4,
    Erofs,
    F2fs,
}

/// Identify a supported Android filesystem without consuming the device.
pub fn probe(device: &mut dyn BlockDevice) -> Result<Option<AndroidFilesystemKind>, FsError> {
    let mut bytes = vec![0u8; PROBE_BYTES];
    read_bytes(device, PROBE_OFFSET, &mut bytes)?;
    if bytes[..4] == EROFS_MAGIC {
        return Ok(Some(AndroidFilesystemKind::Erofs));
    }
    if bytes[..4] == F2FS_MAGIC {
        return Ok(Some(AndroidFilesystemKind::F2fs));
    }
    if bytes[EXT4_MAGIC_OFFSET..EXT4_MAGIC_OFFSET + 2] == EXT4_MAGIC {
        return Ok(Some(AndroidFilesystemKind::Ext4));
    }
    Ok(None)
}

/// Mount an Android logical partition using the detected read-only format.
pub fn mount(
    mut device: Box<dyn BlockDevice>,
) -> Result<(Box<dyn FileSystem>, AndroidFilesystemKind), FsError> {
    let kind = probe(&mut *device)?.ok_or(FsError::InvalidInput)?;
    let filesystem = match kind {
        AndroidFilesystemKind::Ext4 => {
            Box::new(Ext4FileSystem::new(device)?) as Box<dyn FileSystem>
        }
        AndroidFilesystemKind::Erofs => {
            Box::new(ErofsFileSystem::new(device)?) as Box<dyn FileSystem>
        }
        AndroidFilesystemKind::F2fs => {
            Box::new(F2fsFileSystem::new(device)?) as Box<dyn FileSystem>
        }
    };
    Ok((filesystem, kind))
}

fn read_bytes(device: &mut dyn BlockDevice, offset: u64, output: &mut [u8]) -> Result<(), FsError> {
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
