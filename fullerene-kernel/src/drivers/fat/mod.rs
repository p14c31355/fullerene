//! FAT-family filesystem mount dispatcher.
//!
//! The implementation is split by responsibility: block-device adapters,
//! partition discovery, caching, FAT12/16/32, and exFAT.

use alloc::boxed::Box;

use crate::contexts::vfs::FileSystem;
use crate::klog_fmt;
use genome::fs::FsError;

mod block_device;
mod cache;
pub mod exfat;
mod fat32;
mod partition;

pub use block_device::{BlockDevice, BlockError, FatBlockError, FatDevice};
pub use cache::BlockCache;
pub use fat32::FatFileSystem;
pub use partition::{PartitionBlockDevice, find_fat_partition};

/// Detect the volume format and construct the matching VFS implementation.
pub fn mount_device(
    mut device: Box<dyn BlockDevice>,
) -> Result<Box<dyn FileSystem>, (FsError, Option<Box<dyn BlockDevice>>)> {
    let lba = match find_fat_partition(&mut *device) {
        Ok(lba) => lba,
        Err(error) => return Err((error, Some(device))),
    };

    let mut boot = [0; 512];
    if let Err(error) = device.read_sectors(lba, 1, &mut boot) {
        klog_fmt!("filesystem probe failed at LBA {}: {}\n", lba, error);
        return Err((FsError::InvalidInput, Some(device)));
    }

    let partition: Box<dyn BlockDevice> = if lba == 0 {
        device
    } else {
        Box::new(PartitionBlockDevice::new(device, lba))
    };
    let cached: Box<dyn BlockDevice> = Box::new(BlockCache::new(partition, 64));

    if exfat::is_exfat(&boot) {
        exfat::ExFatFileSystem::new(cached)
            .map(|filesystem| Box::new(filesystem) as Box<dyn FileSystem>)
    } else {
        FatFileSystem::new(cached)
            .map(|filesystem| Box::new(filesystem) as Box<dyn FileSystem>)
            .map_err(|error| (error, None))
    }
}
