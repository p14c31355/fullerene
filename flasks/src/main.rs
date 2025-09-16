// fullerene/flasks/src/main.rs
use isobemak::create_disk_and_iso;
use std::{env, io, path::PathBuf, process::Command};

fn main() -> io::Result<()> {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("Failed to get workspace root")
        .to_path_buf();

    // 1. Build fullerene-kernel (no_std)
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
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "fullerene-kernel build failed",
        ));
    }

    let target_dir = workspace_root
        .join("target")
        .join("x86_64-unknown-uefi")
        .join("debug");

    let kernel_path = target_dir.join("fullerene-kernel.efi");

    // 2. Build bellows (no_std)
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
        return Err(io::Error::new(io::ErrorKind::Other, "bellows build failed"));
    }

    let bellows_path = target_dir.join("bellows.efi");

    // 3. Create FAT32 image and ISO using isobemak
    let fat32_img_path = workspace_root.join("fullerene.img");
    let iso_path = workspace_root.join("fullerene.iso");

    create_disk_and_iso(&fat32_img_path, &iso_path, &bellows_path, &kernel_path)?;

    // 4. Run QEMU with the created ISO image
    let ovmf_fd_path = workspace_root
        .join("flasks")
        .join("ovmf")
        .join("RELEASEX64_OVMF.fd");
    let ovmf_vars_fd_path = workspace_root
        .join("flasks")
        .join("ovmf")
        .join("RELEASEX64_OVMF_VARS.fd");

    let qemu_args = [
        "-cdrom",
        ovmf_fd_path.to_str().ok_or_else(|| {
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
        "std", // Standard VGA for EFI apps
        "-serial",
        "file:serial_log.txt", // Serial output for debugging
        "-bios",
        ovmf_fd_path.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "OVMF.fd path contains invalid UTF-8",
            )
        })?,
        "-drive",
        &format!("if=pflash,format=raw,file={}", ovmf_vars_fd_path.display()),
        "-boot",
        "order=d", // Boot from CD-ROM first
        "-D",
        "qemu_log.txt",
        "-no-reboot",
        "-s", // Enable GDB server
        "-S", // Stop CPU at startup
        "-display",
        "gtk,gl=on", // Force display window for EFI output
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

    // Clean up temporary FAT32 image
    std::fs::remove_file(&fat32_img_path)?;

    Ok(())
}
