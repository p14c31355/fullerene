// fullerene/flasks/src/main.rs
mod part_io;
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use gpt::{GptConfig, disk::LogicalBlockSize, partition_types, partition_types::Type};
use std::{
    fs::{self, OpenOptions},
    io::{self, Seek, SeekFrom},
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

    // 3. Create a 64MiB disk image file
    let disk_image_path = Path::new("esp.img");
    if disk_image_path.exists() {
        fs::remove_file(disk_image_path)?;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(disk_image_path)?;
    file.set_len(64 * 1024 * 1024)?; // 64 MiB

    // 4. Create GPT partition table
    let logical_block_size = LogicalBlockSize::Lb512;
    let part_id;
    let sector_size;
    let partition_info;

    {
        let mut gpt = GptConfig::default()
            .writable(true)
            .logical_block_size(logical_block_size)
            .create_from_device(&mut file, None)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        let first_usable_lba = gpt.primary_header().map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?.first_usable;
        let last_usable_lba = gpt.primary_header().map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?.last_usable;
        let part_size_lba = last_usable_lba - first_usable_lba + 1;

        part_id = gpt.add_partition(
            "EFI System Partition",
            part_size_lba * logical_block_size.as_u64(), // size per bytes
            partition_types::EFI,
            0, // flags
            None, // part_alignment
        )
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        
        gpt.update_guid(Some(Uuid::new_v4()));

        // gpt reload
        gpt.write()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        // gpt reload
        file.seek(io::SeekFrom::Start(0))?;
        let gpt_reloaded = GptConfig::default()
            .logical_block_size(logical_block_size)
            .create_from_device(&mut file, None)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        sector_size = logical_block_size.as_u64();
        partition_info = gpt_reloaded.partitions().get(&part_id).unwrap().clone();
    } 

    // 5. Format the partition with FAT32
    let mut part_io_temp = PartitionIo::new(
        file,
        partition_info.first_lba * sector_size,
        (partition_info.last_lba - partition_info.first_lba + 1) * sector_size, // size_lba を計算
    )?;

    fatfs::format_volume(
        &mut part_io_temp,
        FormatVolumeOptions::new().fat_type(FatType::Fat32),
    )?;

    let file = part_io_temp.take_file();

    let mut part_io = PartitionIo::new(
        file,
        partition_info.first_lba * sector_size,
        (partition_info.last_lba - partition_info.first_lba + 1) * sector_size,
    )?;

    // 6. Copy EFI files into the FAT32 filesystem
    let fs = FileSystem::new(&mut part_io, FsOptions::new())?;

    let bellows_efi_src = Path::new("target/x86_64-uefi/release/bellows");
    let kernel_efi_src = Path::new("target/x86_64-uefi/release/fullerene-kernel");

    if !bellows_efi_src.exists() {
        panic!("bellows EFI not found: {}", bellows_efi_src.display());
    }
    if !kernel_efi_src.exists() {
        panic!("fullerene-kernel EFI not found: {}", kernel_efi_src.display());
    }

    copy_to_fat(&fs, bellows_efi_src, "EFI/BOOT/BOOTX64.EFI")?;
    copy_to_fat(&fs, kernel_efi_src, "kernel.efi")?;

    // 7. Copy OVMF_VARS.fd if missing and check for OVMF_CODE.fd
    let ovmf_code = Path::new("/usr/share/OVMF/OVMF_CODE_4M.fd");
    let ovmf_vars = Path::new("./OVMF_VARS.fd");
    let ovmf_vars_src = Path::new("/usr/share/OVMF/OVMF_VARS_4M.fd");

    if !ovmf_code.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} not found. Please ensure the 'ovmf' package is installed.", ovmf_code.display()),
        ));
    }

    if !ovmf_vars.exists() {
        if ovmf_vars_src.exists() {
            fs::copy(ovmf_vars_src, ovmf_vars)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} not found. Please ensure the 'ovmf' package is installed.", ovmf_vars_src.display()),
            ));
        }
    }

    // 8. Run QEMU with the disk image
    let qemu_args = [
        "-drive",
        &format!("if=pflash,format=raw,readonly=on,file={}", ovmf_code.display()),
        "-drive",
        &format!("if=pflash,format=raw,file={}", ovmf_vars.display()),
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
