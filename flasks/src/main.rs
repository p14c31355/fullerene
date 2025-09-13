// fullerene/flasks/src/main.rs
mod part_io;
mod disk;

use std::{
    env,
    fs,
    io::{self},
    path::{Path, PathBuf}, // Import PathBuf
    process::Command,
};
use crate::disk::create_disk_and_iso;

fn main() -> io::Result<()> {
    // Get the workspace root path dynamically
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf(); // Use PathBuf

    // 1. Build fullerene-kernel
    let status = Command::new("cargo")
        .current_dir(&workspace_root) // Ensure command runs from workspace root
        .env("RUST_TARGET_PATH", &workspace_root) // Tell rustc where to find custom targets
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
        .current_dir(&workspace_root) // Ensure command runs from workspace root
        .env("RUST_TARGET_PATH", &workspace_root) // Tell rustc where to find custom targets
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

    // Construct absolute paths to the compiled binaries
    let bellows_binary_path = workspace_root.join("target/x86_64-uefi/release/bellows");
    let kernel_binary_path = workspace_root.join("target/x86_64-uefi/release/fullerene-kernel");

    // No need for temp_efi_dir anymore, as we pass the direct binary paths to create_disk_and_iso
    // and it will handle copying to the FAT image and directly to the ISO.

    let disk_image_path = Path::new("esp.img");
    let iso_path = Path::new("fullerene.iso");

    // Pass the direct binary paths to create_disk_and_iso
    create_disk_and_iso(disk_image_path, iso_path, &bellows_binary_path, &kernel_binary_path)?;

    // 4. Copy OVMF_VARS.fd if missing and check for OVMF_CODE.fd
    let ovmf_code = Path::new("/usr/share/OVMF/OVMF_CODE_4M.fd");
    let ovmf_vars = Path::new("./OVMF_VARS.fd");
    let ovmf_vars_src = Path::new("/usr/share/OVMF/OVMF_VARS_4M.fd");

    if !ovmf_code.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} not found. Ensure 'ovmf' package is installed.", ovmf_code.display()),
        ));
    }

    if !ovmf_vars.exists() {
        if ovmf_vars_src.exists() {
            fs::copy(ovmf_vars_src, ovmf_vars)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} not found. Ensure 'ovmf' package is installed.", ovmf_vars_src.display()),
            ));
        }
    }

    // 5. Run QEMU with disk image
    let qemu_args = [
        "-drive",
        &format!("if=pflash,format=raw,readonly=on,file={}", ovmf_code.display()),
        "-drive",
        &format!("if=pflash,format=raw,file={}", ovmf_vars.display()),
        "-drive",
        &format!("file={},format=raw,if=ide", iso_path.display()), // Boot from ISO
        "-m",
        "512M",
        "-cpu",
        "qemu64,+smap",
        "-serial",
        "stdio",
        "-boot",
        "order=d", // Try to boot from CD-ROM (ISO) first
    ];
    println!("Running QEMU with args: {:?}", qemu_args);
    let qemu_status = Command::new("qemu-system-x86_64")
        .args(&qemu_args)
        .status()?;
    assert!(qemu_status.success());

    Ok(())
}