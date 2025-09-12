// fullerene/flasks/src/main.rs
mod part_io;
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use gpt::{GptConfig, disk::LogicalBlockSize, partition_types};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::Path,
    process::Command,
};
use part_io::{PartitionIo, copy_to_fat};
use uuid::Uuid;

fn main() -> io::Result<()> {
    // 1. Build fullerene-kernel
    let status = Command::new("cargo")
        .args([
            "build",
            "--package",
            "fullerene-kernel",
            "--release",
            "--target",
            "x86_64-uefi.json",
            "-Z",
            "build-std=core,alloc,compiler_builtins",
            "--no-default-features",
            "--features",
            "",
        ])
        .status()?;
    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "fullerene-kernel build failed",
        ));
    }

    // 2. Build bellows
    let status = Command::new("cargo")
        .args([
            "build",
            "--package",
            "bellows",
            "--release",
            "--target",
            "x86_64-uefi.json",
            "-Z",
            "build-std=core,alloc,compiler_builtins",
        ])
        .status()?;
    if !status.success() {
        return Err(io::Error::new(io::ErrorKind::Other, "bellows build failed"));
    }

    // 3. Create disk image
    let disk_img_path = Path::new("esp.img");
    if disk_img_path.exists() {
        fs::remove_file(disk_img_path)?;
    }

    let disk_size_bytes = 128 * 1024 * 1024; // 128 MiB
    let disk_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(disk_img_path)?;

    disk_file.set_len(disk_size_bytes)?;
    disk_file.sync_all()?;

    let gpt_config = GptConfig::new()
        .writable(true)
        .logical_block_size(LogicalBlockSize::Lb512);

    let mut gpt_disk = gpt_config
        .create_from_device(disk_file, None)
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("Failed to create GPT disk: {}", e),
            )
        })?;

    let first_lba = gpt_disk.primary_header().unwrap().first_usable;
    let last_lba = gpt_disk.primary_header().unwrap().last_usable;
    let block_size = gpt_disk.logical_block_size().as_u64();
    let required_esp_bytes = 8 * 1024 * 1024;
    let required_esp_lba = required_esp_bytes / block_size;

    let esp_size_lba = std::cmp::min(required_esp_lba, last_lba - first_lba + 1);
    dbg!(first_lba, last_lba, esp_size_lba);

    gpt_disk
        .add_partition(
            "EFI System Partition",
            esp_size_lba * block_size,
            partition_types::EFI,
            0,
            None,
        )
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to add ESP: {}", e)))?;

    let mut disk_file_after_gpt = gpt_disk
        .write()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to write GPT: {}", e)))?;

    let mut gpt_disk = GptConfig::new()
        .writable(true)
        .logical_block_size(LogicalBlockSize::Lb512)
        .open_from_device(&mut disk_file_after_gpt)
        .map_err(|e| {
            io::Error::new(io::ErrorKind::Other, format!("Failed to reload GPT: {}", e))
        })?;

    let esp_partition_info = gpt_disk
        .partitions()
        .iter()
        .find(|(_, p)| p.part_type_guid == partition_types::EFI)
        .map(|(_, p)| p.clone())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "ESP not found"))?;

    let block_size = gpt_disk.logical_block_size().as_u64();
    let esp_offset_bytes = esp_partition_info.first_lba * block_size;
    let esp_size_bytes =
        (esp_partition_info.last_lba - esp_partition_info.first_lba + 1) * block_size;

    dbg!(esp_offset_bytes, esp_size_bytes);
    if esp_size_bytes < 8 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("ESP too small for FAT32: {} bytes", esp_size_bytes),
        ));
    }

    let fmt_options = FormatVolumeOptions::new()
        .volume_label(*b" FULLERENE ")
        .fat_type(FatType::Fat32);

    let mut esp_io_for_format =
        PartitionIo::new(&mut disk_file_after_gpt, esp_offset_bytes, esp_size_bytes)?;
    fatfs::format_volume(&mut esp_io_for_format, fmt_options)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("FAT format failed: {}", e)))?;

    let esp_io_for_fs =
        PartitionIo::new(&mut disk_file_after_gpt, esp_offset_bytes, esp_size_bytes)?;
    let fs = FileSystem::new(esp_io_for_fs, FsOptions::new()).map_err(|e| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("Failed to open FAT filesystem: {}", e),
        )
    })?;
    fs.root_dir().create_dir("EFI")?;
    fs.root_dir().create_dir("EFI/BOOT")?;
    let boot_dir = fs.root_dir().open_dir("EFI/BOOT")?;

    let mut file = boot_dir.create_file("BOOTX64.EFI")?;
    let efi_binary = std::fs::read("target/x86_64-uefi/release/bellows")?;
    file.write_all(&efi_binary)?;

    // 5. Copy EFI files into FAT32
    let bellows_efi = Path::new("target/x86_64-uefi/release/bellows");
    let kernel_efi = Path::new("target/x86_64-uefi/release/fullerene-kernel");

    if !bellows_efi.exists() {
        panic!("bellows EFI not found: {}", bellows_efi.display());
    }
    if !kernel_efi.exists() {
        panic!("fullerene-kernel EFI not found: {}", kernel_efi.display());
    }

    copy_to_fat(&fs, bellows_efi, "EFI/BOOT/BOOTX64.EFI")?;
    copy_to_fat(&fs, kernel_efi, "kernel.efi")?;

    // Copy OVMF_VARS.fd if missing
    let ovmf_code = "/usr/share/OVMF/OVMF_CODE_4M.fd";
    let ovmf_vars = "./OVMF_VARS.fd";
    if !Path::new(ovmf_vars).exists() {
        fs::copy("/usr/share/OVMF/OVMF_VARS_4M.fd", ovmf_vars)?;
    }

    // Run QEMU
    let qemu_args = [
        "-drive",
        &format!("if=pflash,format=raw,readonly=on,file={}", ovmf_code),
        "-drive",
        &format!("if=pflash,format=raw,file={}", ovmf_vars),
        "-drive",
        "file=esp.img,format=raw,if=ide",
        "-m",
        "512M",
        "-cpu",
        "qemu64,+smap",
        "-serial",
        "stdio",
        "-boot",
        "order=c",
    ];
    println!("Running QEMU with args: {:?}", qemu_args);
    let qemu_status = Command::new("qemu-system-x86_64")
        .args(&qemu_args)
        .status()?;
    assert!(qemu_status.success());

    Ok(())
}