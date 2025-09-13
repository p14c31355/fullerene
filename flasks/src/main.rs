// fullerene/flasks/src/main.rs
mod part_io;
mod disk;

use std::{
    env,
    fs,
    io,
    path::{Path, PathBuf},
    process::Command,
};
use crate::disk::create_disk_and_iso;

/// Build kernel and bellows, create UEFI bootable ISO with xorriso, and run QEMU
fn main() -> io::Result<()> {
    // 0. Workspace root dynamically
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
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
        return Err(io::Error::new(io::ErrorKind::Other, "fullerene-kernel build failed"));
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

    // 6. Use the refactored create_disk_and_iso function
    create_disk_and_iso(
        &disk_image_path,
        &iso_path,
        &bellows_binary_path,
        &kernel_binary_path,
    )?;

    // 7. Prepare OVMF paths
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

    // 8. Run QEMU with the new disk image
    let qemu_args = [
        "-drive",
        &format!("if=pflash,format=raw,readonly=on,file={}", ovmf_code.display()),
        "-drive",
        &format!("if=pflash,format=raw,file={}", ovmf_vars.display()),
        "-cdrom",
        iso_path.to_str().unwrap(),
        "-drive", // Add the new drive argument
        &format!("file={},format=raw", disk_image_path.display()),
        "-m", "512M",
        "-cpu", "qemu64,+smap",
        "-serial", "stdio",
        "-vga", "std",
    ];
    println!("Running QEMU with args: {:?}", qemu_args);

    let qemu_status = Command::new("qemu-system-x86_64")
        .args(&qemu_args)
        .status()?;
    if !qemu_status.success() {
        return Err(io::Error::new(io::ErrorKind::Other, "QEMU execution failed"));
    }

    Ok(())
}
