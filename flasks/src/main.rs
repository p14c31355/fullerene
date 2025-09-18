// fullerene/flasks/src/main.rs
use isobemak::create_disk_and_iso;
use std::{
    env, io,
    path::{Path, PathBuf},
    process::Command,
};

fn main() -> io::Result<()> {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("Failed to get workspace root")
        .to_path_buf();

    // --- 1. Build fullerene-kernel (no_std) ---
    let status = Command::new("cargo")
        .current_dir(&workspace_root)
        .args([
            "+nightly",
            "build",
            "-Zbuild-std=core,alloc",
            "--package",
            "fullerene-kernel",
            "--target",
            "x86_64-unknown-uefi",
            "--profile",
            "dev",
        ])
        .status()?;
    if !status.success() {
        return Err(io::Error::other("fullerene-kernel build failed"));
    }

    let target_dir = workspace_root
        .join("target")
        .join("x86_64-unknown-uefi")
        .join("debug");
    let kernel_path = target_dir.join("fullerene-kernel.efi");

    // --- 2. Build bellows (no_std) ---
    let status = Command::new("cargo")
        .current_dir(&workspace_root)
        .args([
            "+nightly",
            "build",
            "-Zbuild-std=core,alloc",
            "--package",
            "bellows",
            "--target",
            "x86_64-unknown-uefi",
            "--profile",
            "dev",
        ])
        .status()?;
    if !status.success() {
        return Err(io::Error::other("bellows build failed"));
    }
    let bellows_path = target_dir.join("bellows.efi");

    // --- 3. Create ISO using isobemak ---
    let iso_path = workspace_root.join("fullerene.iso");

    create_disk_and_iso(&iso_path, &bellows_path, &kernel_path)?;

    // --- 4. Run QEMU with the created ISO ---
    let ovmf_fd_path = workspace_root
        .join("flasks")
        .join("ovmf")
        .join("RELEASEX64_OVMF.fd");
    let ovmf_vars_fd_path = workspace_root
        .join("flasks")
        .join("ovmf")
        .join("RELEASEX64_OVMF_VARS.fd");

    let ovmf_fd_drive = format!(
        "if=pflash,format=raw,readonly=on,file={}",
        ovmf_fd_path.display()
    );
    let ovmf_vars_fd_drive = format!("if=pflash,format=raw,file={}", ovmf_vars_fd_path.display());

    let qemu_args = [
        "-cdrom",
        iso_path.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "ISO path contains invalid UTF-8",
            )
        })?,
        "-m",
        "512M",
        "-cpu",
        "qemu64,+smap",
        "-vga",
        "std",
        "-serial",
        "stdio",
        "-drive",
        &ovmf_fd_drive,
        "-drive",
        &ovmf_vars_fd_drive,
        "-boot",
        "order=d",
        "-display",
        "sdl",
        "-no-reboot",
        "-d",
        "guest_errors",
        "-D",
        "qemu_log.txt",
        "-s",
    ];

    let qemu_status = Command::new("qemu-system-x86_64")
        .args(qemu_args)
        .status()?;

    if !qemu_status.success() {
        return Err(io::Error::other("QEMU execution failed"));
    }

    Ok(())
}
