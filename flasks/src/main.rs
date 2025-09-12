// fullerene/flasks/src/main.rs
mod part_io;
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use gpt::{GptConfig, disk::LogicalBlockSize, partition_types};
use std::{
    fs::{self, OpenOptions},
    io::{self},
    path::Path,
    process::Command,
    error::Error,
};
use part_io::{PartitionIo, copy_to_fat};

fn main() -> Result<(), Box<dyn Error>> {
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
        ).into());
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
        return Err(io::Error::new(io::ErrorKind::Other, "bellows build failed").into());
    }

    // 3. Create a 64MiB disk image file
    let disk_image_path = Path::new("esp.img");
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
    let mut gpt = GptConfig::new()
        .writable(true)
        .logical_block_size(logical_block_size)
        .create_from_device(file, None)?;

    let part_size_lba = gpt.find_free_sectors().get(0).map(|(_, len)| *len).unwrap_or(0);
    let part_id = gpt.add_partition(
        "EFI System Partition",
        part_size_lba,
        partition_types::EFI,
        0,
        None,
    )?;

    gpt.write_inplace()?;

    let sector_size = logical_block_size.as_usize() as u64;
    let first_lba = gpt.partitions().get(&part_id).unwrap().first_lba;
    let last_lba = gpt.partitions().get(&part_id).unwrap().last_lba;

    // 5. Format the partition with FAT32
    let mut part_io = PartitionIo::new(
        gpt.device_mut(),
        first_lba * sector_size,
        (last_lba - first_lba + 1) * sector_size,
    )?;

    fatfs::format_volume(
        &mut part_io,
        FormatVolumeOptions::new().fat_type(FatType::Fat32),
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

    // 7. Copy OVMF_VARS.fd if missing
    let ovmf_code = "/usr/share/OVMF/OVMF_CODE_4M.fd";
    let ovmf_vars = "./OVMF_VARS.fd";
    if !Path::new(ovmf_vars).exists() {
        fs::copy("/usr/share/OVMF/OVMF_VARS_4M.fd", ovmf_vars)?;
    }

    // 8. Run QEMU with the ISO image
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
        // "-boot",
        // "order=c",
    ];
    println!("Running QEMU with args: {:?}", qemu_args);
    let qemu_status = Command::new("qemu-system-x86_64")
        .args(&qemu_args)
        .status()?;
    assert!(qemu_status.success());

    Ok(())
}
