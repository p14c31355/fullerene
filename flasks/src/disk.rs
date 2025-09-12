// fullerene/flasks/src/disk.rs
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use gpt::{GptConfig, disk::LogicalBlockSize, partition_types};
use std::{
    fs::{self, OpenOptions, File},
    io::{self, Seek, SeekFrom},
    path::{Path, PathBuf},
};
use hadris_iso::{IsoImage, FileInput, FormatOptions, Strictness, BootOptions, BootEntryOptions, boot::EmulationType};
use crate::part_io::{PartitionIo, copy_to_fat};

/// Creates both a raw disk image and a UEFI-bootable ISO
pub fn create_disk_and_iso(
    disk_image_path: &Path,
    iso_path: &Path,
    bellows_efi_src: &Path,
    kernel_efi_src: &Path,
) -> io::Result<()> {
    // 1. Create raw disk image and get the file handle
    let mut disk_file = create_disk_image(disk_image_path, bellows_efi_src, kernel_efi_src)?;
    let efi = "EFI/BOOT/BOOTX64.EFI".to_string();
    let iso = "temp_iso_stage".to_string();

    // 2. Create UEFI ISO from disk image using hadris-iso
    let efi_boot_path = Path::new(&efi);
    
    // Create a temporary directory to stage the ISO contents
    let temp_iso_dir = Path::new(&iso);
    if temp_iso_dir.exists() {
        fs::remove_dir_all(temp_iso_dir)?;
    }
    fs::create_dir_all(temp_iso_dir)?;

    // Read GPT and find the EFI partition
    disk_file.seek(SeekFrom::Start(0))?;
    let gpt = GptConfig::default()
        .logical_block_size(LogicalBlockSize::Lb512)
        .open_from_device(&mut disk_file)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to read GPT: {}", e)))?;

    let efi_partition = gpt.partitions().iter()
        .find(|&(_, p)| p.part_type_guid == partition_types::EFI)
        .map(|(_, p)| p)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "EFI partition not found in disk image"))?;

    let sector_size = LogicalBlockSize::Lb512.as_u64();
    let efi_offset = efi_partition.first_lba * sector_size;
    let efi_size = (efi_partition.last_lba - efi_partition.first_lba + 1) * sector_size;

    // Create a PartitionIo for the EFI partition
    let mut efi_part_io = PartitionIo::new(
        disk_file,
        efi_offset,
        efi_size,
    )?;

    // Mount the FAT32 filesystem from the EFI partition
    let efi_fs = FileSystem::new(&mut efi_part_io, FsOptions::new())?;

    // Copy contents of the EFI partition to the temporary staging directory
    let mut stack: Vec<(fatfs::Dir<'_, &mut PartitionIo>, PathBuf)> = vec![(efi_fs.root_dir(), PathBuf::new())];

    while let Some((current_dir, current_path)) = stack.pop() {
        for entry in current_dir.iter().filter_map(|e| e.ok()) {
            let entry_name = entry.file_name();
            let entry_path = current_path.join(&entry_name);
            let dest_path = temp_iso_dir.join(&entry_path);

            if entry.is_dir() {
                fs::create_dir_all(&dest_path)?;
                stack.push((entry.to_dir(), entry_path));
            } else {
                let mut src_file = entry.to_file();
                let mut dest_file = fs::File::create(&dest_path)?;
                io::copy(&mut src_file, &mut dest_file)?;
            }
        }
    }

    let boot_entry_options = BootEntryOptions {
        load_size: 0, // UEFI doesn't need load_size
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

    let options = FormatOptions::new()
        .with_files(FileInput::from_fs(temp_iso_dir.to_path_buf()).unwrap())
        .with_volume_name("FULLERENE".to_string())
        .with_strictness(Strictness::Default) // Use default strictness
        .with_boot_options(boot_options);

    IsoImage::format_file(iso_path.to_path_buf(), options)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to create ISO: {}", e)))?;

    // Clean up the temporary directory
    fs::remove_dir_all(temp_iso_dir)?;

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

    // Create or truncate disk image (128 MiB)
    if disk_image_path.exists() {
        fs::remove_file(disk_image_path)?;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(disk_image_path)?;
    file.set_len(128 * 1024 * 1024)?; // 128 MiB

    let logical_block_size = LogicalBlockSize::Lb512;
    let sector_size = logical_block_size.as_u64();

    // Create GPT and EFI partition
    let partition_info = create_gpt_partition(&mut file, logical_block_size)?;

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
    
{
    // Mount filesystem
    let fs = FileSystem::new(&mut part_io, FsOptions::new())?;

    // Ensure EFI/BOOT directories exist
    let root_dir = fs.root_dir();
    let efi_dir = root_dir.open_dir("EFI").or_else(|_| root_dir.create_dir("EFI"))?;
    let _boot_dir = efi_dir.open_dir("BOOT").or_else(|_| efi_dir.create_dir("BOOT"))?;

    // Copy EFI files into EFI/BOOT
    copy_to_fat(&fs, bellows_efi_src, Path::new("EFI/BOOT/BOOTX64.EFI"))?;
    copy_to_fat(&fs, kernel_efi_src, Path::new("EFI/BOOT/kernel.efi"))?;
}
    // Get back the file handle
    let file = part_io.into_inner()?;
    Ok(file)
}

/// Creates a GPT partition table with a single EFI System Partition (16 MiB)
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
