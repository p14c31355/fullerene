// fullerene/flasks/src/disk.rs
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use gpt::{GptConfig, disk::LogicalBlockSize, partition_types};
use std::{
    fs::{self, OpenOptions},
    io,
    path::Path,
};
use crate::part_io::{PartitionIo, copy_to_fat};

/// Creates and initializes the disk image with GPT, FAT32 filesystem,
/// ensures EFI/BOOT directories, and copies EFI files.
pub fn create_disk_image(
    disk_image_path: &Path,
    bellows_efi_src: &Path,
    kernel_efi_src: &Path,
) -> io::Result<()> {
    // Check EFI files exist
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

    // Create or truncate disk image (128 MiB)
    if disk_image_path.exists() {
        fs::remove_file(disk_image_path)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(disk_image_path)?;
    file.set_len(128 * 1024 * 1024)?; // 128 MiB

    let logical_block_size = LogicalBlockSize::Lb512;
    let sector_size = logical_block_size.as_u64();

    // Create GPT and EFI partition
    let partition_info = create_gpt_partition(&file, logical_block_size)?;

    // Initialize PartitionIo for FAT32
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

    // Ensure EFI/BOOT directories exist
    let root_dir = fs.root_dir();
    let efi_dir = root_dir.open_dir("EFI").or_else(|_| root_dir.create_dir("EFI"))?;
    let _boot_dir = efi_dir.open_dir("BOOT").or_else(|_| efi_dir.create_dir("BOOT"))?;

    // Copy EFI files into EFI/BOOT
    copy_to_fat(&fs, bellows_efi_src, "EFI/BOOT/BOOTX64.EFI")?;
    copy_to_fat(&fs, kernel_efi_src, "EFI/BOOT/kernel.efi")?;

    Ok(())
}

/// Creates a GPT partition table with a single EFI System Partition (16 MiB)
fn create_gpt_partition(
    file: &std::fs::File,
    logical_block_size: LogicalBlockSize,
) -> io::Result<gpt::partition::Partition> {
    let mut gpt = GptConfig::default()
        .writable(true)
        .logical_block_size(logical_block_size)
        .create_from_device(file, None)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    let header = gpt.primary_header()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    let first_usable = header.first_usable;
    let last_usable = header.last_usable;

    // Calculate 16 MiB partition size in LBAs
    let sector_size = logical_block_size.as_u64();
    let fat32_size_bytes: u64 = 16 * 1024 * 1024;
    let fat32_size_lba: u64 = (fat32_size_bytes + sector_size - 1) / sector_size;

    if first_usable + fat32_size_lba > last_usable {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Disk too small for 16 MiB EFI partition",
        ));
    }

    // Add EFI partition (LBA in u64)
    let temp_part_id: u32 = gpt.add_partition(
        "EFI System Partition",
        fat32_size_lba,
        partition_types::EFI,
        0,
        None,
    ).map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    let partition = gpt.partitions().get(&temp_part_id)
        .expect("Added partition not found")
        .clone();

    gpt.write().map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    Ok(partition)
}
