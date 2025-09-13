// fullerene/flasks/src/disk.rs
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use gpt::{GptConfig, disk::LogicalBlockSize, partition_types};
use std::{
    fs::{self, OpenOptions, File},
    io::{self},
    path::{Path, PathBuf},
};
use hadris_iso::{IsoImage, FileInput, FormatOptions, Strictness, BootOptions, BootEntryOptions, boot::EmulationType};
use crate::part_io::{PartitionIo, copy_to_fat};

/// Creates both a raw disk image and a UEFI-bootable ISO
pub fn create_disk_and_iso(
    disk_image_path: &Path,
    iso_path: &Path,
    bellows_efi_src: &Path, // These are now direct paths to the compiled binaries
    kernel_efi_src: &Path,  // These are now direct paths to the compiled binaries
) -> io::Result<()> {
    // 1. Create raw disk image and populate it with EFI files
    // This function now handles creating the esp.img, partitioning, formatting,
    // and copying the EFI binaries into it.
    let _disk_file = create_disk_image(disk_image_path, bellows_efi_src, kernel_efi_src)?;

    // 2. Create UEFI ISO from the *already prepared* EFI files
    let efi_boot_path = PathBuf::from("EFI/BOOT/BOOTX64.EFI"); // Destination path within the ISO

    let boot_entry_options = BootEntryOptions {
        load_size: 0,
        boot_image_path: efi_boot_path.to_string_lossy().into_owned(),
        boot_info_table: false,
        grub2_boot_info: false,
        emulation: EmulationType::NoEmulation,
    };

    let boot_options = BootOptions {
        write_boot_catalogue: true,
        default: boot_entry_options,
        entries: Vec::new(),
    };

    // No need for a temporary staging directory for ISO creation anymore,
    // as we are adding files individually.

    let options = FormatOptions::new()
        // CRITICAL CHANGE: Explicitly add files with their ISO 9660 compliant names
        .with_files(
            FileInput::new()
                .add_file(bellows_efi_src.to_path_buf(), PathBuf::from("EFI/BOOT/BOOTX64.EFI"))?
                .add_file(kernel_efi_src.to_path_buf(), PathBuf::from("EFI/BOOT/KERNEL.EFI"))?
        )
        .with_volume_name("FULLERENE".to_string())
        .with_strictness(Strictness::Default) // Can revert to Default, as names are now compliant
        .with_boot_options(boot_options);

    IsoImage::format_file(iso_path.to_path_buf(), options)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to create ISO: {}", e)))?;
    
    Ok(())
}

/// Creates and initializes the raw disk image with GPT, FAT32 filesystem,
/// ensures EFI/BOOT directories, and copies EFI files.
fn create_disk_image(
    disk_image_path: &Path,
    bellows_efi_src: &Path,
    kernel_efi_src: &Path,
) -> io::Result<File> {
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

    // Create or truncate disk image (256 MiB)
    if disk_image_path.exists() {
        fs::remove_file(disk_image_path)?;
    }
    let mut file = OpenOptions::new() // No need for mut here, as it's passed to PartitionIo
        .read(true)
        .write(true)
        .create(true)
        .open(disk_image_path)?;
    file.set_len(256 * 1024 * 1024)?; // 256 MiB

    let logical_block_size = LogicalBlockSize::Lb512;
    let sector_size = logical_block_size.as_u64();

    // Create GPT and EFI partition
    let partition_info = create_gpt_partition(&mut file, logical_block_size)?;

    // Initialize PartitionIo for FAT32
    let mut part_io = PartitionIo::new(
        file, // file is consumed here
        partition_info.first_lba * sector_size,
        (partition_info.last_lba - partition_info.first_lba + 1) * sector_size,
    )?;

    // Format FAT32
    fatfs::format_volume(
        &mut part_io,
        FormatVolumeOptions::new().fat_type(FatType::Fat32),
    )?;
    
    {
        // Mount filesystem
        let fs = FileSystem::new(&mut part_io, FsOptions::new())?;

        // Ensure EFI/BOOT directories exist
        let root_dir = fs.root_dir();
        // Copy EFI files into EFI/BOOT
        copy_to_fat(&root_dir, bellows_efi_src, "EFI/BOOT/BOOTX64.EFI")?;
        // CRITICAL CHANGE: Ensure kernel.efi is copied as KERNEL.EFI for FAT32 compliance
        copy_to_fat(&root_dir, kernel_efi_src, "EFI/BOOT/KERNEL.EFI")?;
    }

    // Get back the file handle
    let file = part_io.into_inner()?;
    Ok(file)
}

/// Creates a GPT partition table with a single EFI System Partition (64 MiB)
fn create_gpt_partition(
    file: &mut std::fs::File,
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

    // Calculate 64 MiB partition size in LBAs
    let sector_size = logical_block_size.as_u64();
    let fat32_size_bytes: u64 = 64 * 1024 * 1024;
    let fat32_size_lba: u64 = (fat32_size_bytes + sector_size - 1) / sector_size;

    if first_usable + fat32_size_lba > last_usable {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Disk too small for 64 MiB EFI partition",
        ));
    }

    // Add EFI partition
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