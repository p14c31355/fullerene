// fullerene/flasks/src/disk.rs
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use gpt::{GptConfig, disk::LogicalBlockSize, partition_types};
use std::{
    fs::{self, OpenOptions},
    io,
    path::Path,
};
use crate::part_io::{PartitionIo, copy_to_fat};

/// Creates and initializes the disk image with GPT, FAT32 filesystem, and copies the boot files.
pub fn create_disk_image(
    disk_image_path: &Path,
    bellows_efi_src: &Path,
    kernel_efi_src: &Path,
) -> io::Result<()> {
    // Ensure EFI files exist
    if !bellows_efi_src.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("bellows EFI not found: {}", bellows_efi_src.display()),
        ));
    }
    if !kernel_efi_src.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("fullerene-kernel EFI not found: {}", kernel_efi_src.display()),
        ));
    }

    // Create or truncate disk image to 64MiB
    if disk_image_path.exists() {
        fs::remove_file(disk_image_path)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(disk_image_path)?;
    file.set_len(64 * 1024 * 1024)?; // 64 MiB

    let logical_block_size = LogicalBlockSize::Lb512;
    let sector_size = logical_block_size.as_u64();

    // Create GPT and EFI partition
    let partition_info = create_gpt_partition(&file, logical_block_size)?;

    // Initialize PartitionIo for FAT32 formatting
    let mut part_io = PartitionIo::new(
        file,
        partition_info.first_lba * sector_size,
        (partition_info.last_lba - partition_info.first_lba + 1) * sector_size,
    )?;

    // Format FAT32
    fatfs::format_volume(
        &mut part_io,
        FormatVolumeOptions::new().fat_type(FatType::Fat32),
    )?;

    // Mount filesystem
    let fs = FileSystem::new(&mut part_io, FsOptions::new())?;

    // Ensure EFI/BOOT directory exists inside FAT
    fs.root_dir().create_dir("EFI")?;
    fs.root_dir().open_dir("EFI")?.create_dir("BOOT")?;

    // Copy EFI files
    copy_to_fat(&fs, bellows_efi_src, "EFI/BOOT/BOOTX64.EFI")?;
    copy_to_fat(&fs, kernel_efi_src, "kernel.efi")?;

    Ok(())
}

/// Creates a GPT partition table with a single EFI System Partition (16 MiB)
fn create_gpt_partition(file: &std::fs::File, logical_block_size: LogicalBlockSize) -> io::Result<gpt::partition::Partition> {
    let mut gpt = GptConfig::default()
        .writable(true)
        .logical_block_size(logical_block_size)
        .create_from_device(file, None)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    let header = gpt.primary_header()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    let first_usable = header.first_usable;
    let last_usable = header.last_usable;

    // Set FAT32 ESP to 16 MiB
    let sector_size = logical_block_size.as_u64();
    let fat32_size_bytes = 16 * 1024 * 1024; // 16 MiB
    let fat32_size_lba = (fat32_size_bytes + sector_size - 1) / sector_size;

    if first_usable + fat32_size_lba > last_usable {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Disk too small for 16 MiB EFI partition",
        ));
    }

    let part_id = gpt.add_partition(
        "EFI System Partition",
        fat32_size_lba,
        partition_types::EFI,
        0,
        None,
    ).map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    let partition = gpt.partitions().get(&part_id).unwrap().clone();

    gpt.write()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    Ok(partition)
}
