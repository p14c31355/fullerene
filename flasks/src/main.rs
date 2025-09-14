// fullerene/flasks/src/main.rs
mod disk;

use crate::disk::create_disk_and_iso;
use std::{env, fs::File, io, path::PathBuf, process::Command};

/// Build kernel and bellows, create UEFI bootable ISO with xorriso, and run QEMU
fn main() -> io::Result<()> {
    // 0. Workspace root dynamically
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("Failed to get workspace root")
        .to_path_buf();

    // 1. Build fullerene-kernel
    let status = Command::new("cargo")
        .current_dir(&workspace_root)
        .env("RUST_TARGET_PATH", &workspace_root)
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
            "--target-dir",
            "target",
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
        .current_dir(&workspace_root)
        .env("RUST_TARGET_PATH", &workspace_root)
        .args([
            "build",
            "--package",
            "bellows",
            "--release",
            "--target",
            "x86_64-uefi.json",
            "-Z",
            "build-std=core,alloc,compiler_builtins",
            "--target-dir",
            "target",
        ])
        .status()?;
    if !status.success() {
        return Err(io::Error::new(io::ErrorKind::Other, "bellows build failed"));
    }

    // 3. Paths to binaries
    let bellows_path = workspace_root.join("target/x86_64-uefi/release/bellows");
    let kernel_path = workspace_root.join("target/x86_64-uefi/release/fullerene-kernel");

    let mut bellows_file = File::open(&bellows_path)?;
    let mut kernel_file = File::open(&kernel_path)?;

    // 4. FAT32 disk image path
    let disk_image_path = workspace_root.join("fullerene.img");
    let iso_path = workspace_root.join("fullerene.iso");

    println!("Disk Image Path: {}", disk_image_path.display());
    println!("ISO Path: {}", iso_path.display());
    println!("ISO Exists before QEMU: {}", iso_path.exists());

    // 5. Create FAT32 image containing EFI binaries
    create_disk_and_iso(&disk_image_path, &iso_path, &mut bellows_file, &mut kernel_file)?;

    println!("ISO Exists after creation: {}", iso_path.exists());

    // 6. Prepare OVMF paths
    let ovmf_dir = workspace_root.join("flasks").join("ovmf");
    let ovmf_code = ovmf_dir.join("RELEASEX64_OVMF.fd");
    let ovmf_vars = ovmf_dir.join("RELEASEX64_OVMF_VARS.fd");

    if !ovmf_code.exists() || !ovmf_vars.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "OVMF files not found in flasks/ovmf/",
        ));
    }

    // 7. Run QEMU with FAT32 image as direct boot
    let qemu_args = [
        "-drive",
        &format!(
            "if=pflash,format=raw,readonly=on,file={}",
            ovmf_code.display()
        ),
        "-drive",
        &format!("if=pflash,format=raw,file={}", ovmf_vars.display()),
        "-cdrom",
        &format!("{}", iso_path.display()), // Boot from ISO
        "-boot",
        "once=d",
        "-m",
        "512M",
        "-cpu",
        "qemu64,+smap",
        "-serial",
        "stdio",
        "-vga",
        "std",
    ];

    let qemu_status = Command::new("qemu-system-x86_64")
        .args(&qemu_args)
        .status()?;
    if !qemu_status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "QEMU execution failed",
        ));
    }

    Ok(())
}
