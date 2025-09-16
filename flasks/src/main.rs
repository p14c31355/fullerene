// fullerene/flasks/src/main.rs
use std::{
    env,
    io,
    path::PathBuf,
    process::Command,
};
use isobemak::create_disk_and_iso;

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
            "x86_64-unknown-none",
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
        .join("x86_64-unknown-none")
        .join("release");

    let kernel_path = target_dir.join("fullerene-kernel");

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
            "x86_64-unknown-none",
        ])
        .status()?;
    if !status.success() {
        return Err(io::Error::new(io::ErrorKind::Other, "bellows build failed"));
    }

    let bellows_path = target_dir.join("bellows");

    // 3. Create FAT32 image and ISO using isobemak
    let fat32_img_path = workspace_root.join("fullerene.img");
    let iso_path = workspace_root.join("fullerene.iso");

    create_disk_and_iso(
        &fat32_img_path,
        &iso_path,
        &bellows_path,
        &kernel_path,
    )?;

    // 4. Run QEMU with the created ISO image
    let ovmf_fd_path = workspace_root.join("flasks").join("ovmf").join("RELEASEX64_OVMF.fd");
    let ovmf_vars_fd_path = workspace_root.join("flasks").join("ovmf").join("RELEASEX64_OVMF_VARS.fd");

    let qemu_args = [
        "-cdrom",
        iso_path.to_str().expect("Failed to convert ISO path to string"),
        "-m",
        "512M",
        "-cpu",
        "qemu64,+smap",
        "-vga",
        "std",               // Enable standard VGA emulation
        "-nographic",
        "-serial",
        "file:serial_log.txt", // Redirect serial output to file
        "-bios",
        ovmf_fd_path.to_str().expect("Failed to convert OVMF.fd path to string"),
        "-drive",
        &format!("if=pflash,format=raw,file={}", ovmf_vars_fd_path.display()),
        "-debugcon",
        "file:qemu_log.txt", // Add debug output to file
        "-no-reboot",        // Prevent QEMU from rebooting on panic
        "-monitor",
        "stdio",             // Connect QEMU monitor to stdio
        "-S",                // Stop CPU at startup
        "-s",                // Enable GDB server
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
