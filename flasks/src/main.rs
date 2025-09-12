// fullerene/flasks/src/main.rs
mod part_io;
mod disk;

use std::{
    fs,
    io::{self},
    path::Path,
    process::Command,
};
use crate::disk::create_disk_and_iso;

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
    let iso_path = Path::new("fullerene.iso");
    let bellows_efi_src = Path::new("target/x86_64-uefi/release/bellows");
    let kernel_efi_src = Path::new("target/x86_64-uefi/release/fullerene-kernel");

    create_disk_and_iso(disk_image_path, iso_path, bellows_efi_src, kernel_efi_src)?;

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
        &format!("file={},format=raw,if=ide", iso_path.display()),
        "-m",
        "512M",
        "-cpu",
        "qemu64,+smap",
        "-serial",
        "stdio",
        "-boot",
        "order=d",
    ];
    println!("Running QEMU with args: {:?}", qemu_args);
    let qemu_status = Command::new("qemu-system-x86_64")
        .args(&qemu_args)
        .status()?;
    assert!(qemu_status.success());

    Ok(())
}
