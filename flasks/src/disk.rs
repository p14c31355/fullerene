// fullerene/flasks/src/disk.rs
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use gpt::{GptConfig, disk::LogicalBlockSize, partition_types};
use std::{
    fs::{self, OpenOptions},
    io::{self},
    path::Path,
};
use crate::part_io::{PartitionIo, copy_to_fat};

/// Creates and initializes the disk image with GPT, FAT32 filesystem, and copies the boot files.
pub fn create_disk_image(
    disk_image_path: &Path,
    bellows_efi_src: &Path,
    kernel_efi_src: &Path,
) -> io::Result<()> {
    // 3. Create a 64MiB disk image file
    if disk_image_path.exists() {
        fs::remove_file(disk_image_path)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(disk_image_path)?;
    file.set_len(64 * 1024 * 1024)?; // 64 MiB

    // 4. Create GPT partition table
    let logical_block_size = LogicalBlockSize::Lb512;
    let sector_size = logical_block_size.as_u64();
    let partition_info;
    let mut file_clone = file;

    {
        let mut gpt = GptConfig::default()
            .writable(true)
            .logical_block_size(logical_block_size)
            .create_from_device(&mut file_clone, None)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        let first_usable_lba = gpt.primary_header().map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?.first_usable;
        let last_usable_lba = gpt.primary_header().map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?.last_usable;
        let part_size_lba = last_usable_lba - first_usable_lba + 1;

        let part_id = gpt.add_partition(
            "EFI System Partition",
            part_size_lba,
            partition_types::EFI,
            0, // flags
            None, // part_alignment
        )
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        partition_info = gpt.partitions().get(&part_id).unwrap().clone();

        gpt.write()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    }
    
    // 5. Format the partition with FAT32
    let mut part_io_temp = PartitionIo::new(
        file_clone,
        partition_info.first_lba * sector_size,
        (partition_info.last_lba - partition_info.first_lba + 1) * sector_size,
    )?;
    fatfs::format_volume(
        &mut part_io_temp,
        FormatVolumeOptions::new().fat_type(FatType::Fat32),
    )?;

    // Take back file ownership
    let file = part_io_temp.take_file();

    // Recreate PartitionIo
    let mut part_io = PartitionIo::new(
        file,
        partition_info.first_lba * sector_size,
        (partition_info.last_lba - partition_info.first_lba + 1) * sector_size,
    )?;

    // 6. Copy EFI files into FAT32 filesystem
    let fs = FileSystem::new(&mut part_io, FsOptions::new())?;

    if !bellows_efi_src.exists() {
        panic!("bellows EFI not found: {}", bellows_efi_src.display());
    }
    if !kernel_efi_src.exists() {
        panic!("fullerene-kernel EFI not found: {}", kernel_efi_src.display());
    }

    copy_to_fat(&fs, bellows_efi_src, "EFI/BOOT/BOOTX64.EFI")?;
    copy_to_fat(&fs, kernel_efi_src, "kernel.efi")?;

    Ok(())
}
