// fullerene/flasks/src/main.rs
mod disk;

use crate::disk::create_disk_and_iso;
use std::{env, io, path::PathBuf, process::Command};

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

    // 3. Absolute paths to binaries
    let bellows_binary_path = workspace_root.join("target/x86_64-uefi/release/bellows");
    let kernel_binary_path = workspace_root.join("target/x86_64-uefi/release/fullerene-kernel");

    // 4. Set the new disk image path at the project root
    let disk_image_path = workspace_root.join("fullerene.img");

    // 5. Set the ISO path
    let iso_path = workspace_root.join("fullerene.iso");

    println!("bellows path: {:?}", bellows_binary_path.canonicalize()?);
    println!("kernel path: {:?}", kernel_binary_path.canonicalize()?);

    // 6. Use the refactored create_disk_and_iso function
        let bellows_file = std::fs::File::open(&bellows_binary_path)?;
    let kernel_file = std::fs::File::open(&kernel_binary_path)?;

    println!("bellows file opened successfully");
    println!("kernel file opened successfully");

    if let Err(e) = create_disk_and_iso(
        &disk_image_path,
        &iso_path,
        &bellows_file,
        &kernel_file,
    ) {
        eprintln!("Error creating disk and iso: {:?}", e);
        return Err(e);
    }

    // 7. Prepare OVMF paths from the fixed local directory
    let ovmf_dir = workspace_root.join("flasks").join("ovmf");
    let ovmf_code = ovmf_dir.join("RELEASEX64_OVMF.fd");
    let ovmf_vars = ovmf_dir.join("RELEASEX64_OVMF_VARS.fd");

    if !ovmf_code.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{} not found. Please ensure it exists in the specified directory.",
                ovmf_code.display()
            ),
        ));
    }

    if !ovmf_vars.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{} not found. Please ensure it exists in the specified directory.",
                ovmf_vars.display()
            ),
        ));
    }

    // 8. Run QEMU with the new disk image
    let qemu_args = [
        "-drive",
        &format!(
            "if=pflash,format=raw,readonly=on,file={}",
            ovmf_code.display()
        ),
        "-drive",
        &format!("if=pflash,format=raw,file={}", ovmf_vars.display()),
        "-drive",
        &format!("file={},format=raw", disk_image_path.display()),
        "-boot",
        "d",
        "-m",
        "512M",
        "-cpu",
        "qemu64,+smap",
        "-serial",
        "stdio",
        "-vga",
        "std",
    ];
    println!("Running QEMU with args: {:?}", qemu_args);

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
