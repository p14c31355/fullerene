// fullerene/flasks/src/main.rs
use std::{env, io, path::PathBuf, process::Command};

fn main() -> io::Result<()> {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("Failed to get workspace root")
        .to_path_buf();

    // 1. Build fullerene-kernel
    let status = Command::new("cargo")
        .current_dir(&workspace_root)
        .args([
            "build",
            "-Zbuild-std",
            "--package",
            "fullerene-kernel",
            "--release",
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

    let kernel_path = workspace_root
        .join("target")
        .join("x86_64-unknown-none")
        .join("release")
        .join("fullerene-kernel");

    // 2. Build bellows
    let status = Command::new("cargo")
        .current_dir(&workspace_root)
        .args([
            "build",
            "-Zbuild-std",
            "--package",
            "bellows",
            "--release",
            "--target",
            "x86_64-unknown-none",
        ])
        .status()?;
    if !status.success() {
        return Err(io::Error::new(io::ErrorKind::Other, "bellows build failed"));
    }

    let bellows_path = workspace_root
        .join("target")
        .join("x86_64-unknown-none")
        .join("release")
        .join("bellows");

    // 3. Create a simple disk image
    let disk_img_path = workspace_root.join("fullerene.img");
    let mut file = std::fs::File::create(&disk_img_path)?;
    // Write bellows (bootloader) to the beginning of the disk image
    let bellows_bytes = std::fs::read(&bellows_path)?;
    io::Write::write_all(&mut file, &bellows_bytes)?;
    // For simplicity, we'll just append the kernel for now. A real bootloader
    // would load the kernel from a known location on the disk.
    let kernel_bytes = std::fs::read(&kernel_path)?;
    io::Write::write_all(&mut file, &kernel_bytes)?;

    // 4. Run QEMU with the raw disk image
    let qemu_args = [
        "-drive",
        &format!("format=raw,file={}", disk_img_path.display()),
        "-boot",
        "a", // Boot from floppy/hard disk
        "-m",
        "512M",
        "-cpu",
        "qemu64,+smap",
        "-nographic",
        "-serial",
        "mon:stdio",
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