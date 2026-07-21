use alloc::boxed::Box;

use crate::block::BlockDevice;
use crate::fs::FsError;
use crate::vfs::FileSystem;

mod block_device;
mod cache;
pub mod exfat;
mod fat32;
mod partition;

pub use block_device::{FatBlockError, FatDevice};
pub use cache::BlockCache;
pub use fat32::FatFileSystem;
pub use partition::{PartitionBlockDevice, find_fat_partition};

use block_device::read_boot_sector;

pub fn mount_device(
    mut device: Box<dyn BlockDevice>,
) -> Result<Box<dyn FileSystem>, (FsError, Option<Box<dyn BlockDevice>>)> {
    let info = match find_fat_partition(&mut *device) {
        Ok(info) => info,
        Err(error) => return Err((error, Some(device))),
    };

    let boot = match read_boot_sector(&mut *device, info.start_lba) {
        Ok(boot) => boot,
        Err(error) => {
            log::info!(
                "filesystem probe failed at LBA {}: {:?}",
                info.start_lba,
                error
            );
            return Err((FsError::InvalidInput, Some(device)));
        }
    };

    let partition: Box<dyn BlockDevice> = if info.start_lba == 0 {
        device
    } else {
        Box::new(PartitionBlockDevice::new(
            device,
            info.start_lba,
            info.total_sectors,
        ))
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
