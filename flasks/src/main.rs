// fullerene/flasks/src/main.rs
use isobemak::{create_fat32_image, create_iso_from_img};
use std::{env, io, path::PathBuf, process::Command};

fn main() -> io::Result<()> {
    // Workspace root (one level up from flasks/)
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("Failed to get workspace root")
        .to_path_buf();

    // 1. Build fullerene-kernel
    let status = Command::new("cargo")
        .current_dir(&workspace_root)
        .env(
            "RUSTFLAGS",
            format!(
                "-C link-arg=-T{}",
                workspace_root.join("linker.ld").display()
            ),
        )
        .args([
            "build",
            "--package",
            "fullerene-kernel",
            "--release",
            "--target",
            "x86_64-unknown-none",
            "-Z",
            "build-std=core,alloc,compiler_builtins",
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
        .join("fullerene-kernel.efi");

    // 2. Build bellows
    let status = Command::new("cargo")
        .current_dir(&workspace_root)
        .env(
            "RUSTFLAGS",
            format!(
                "-C link-arg=-T{}",
                workspace_root.join("linker.ld").display()
            ),
        )
        .args([
            "build",
            "--package",
            "bellows",
            "--release",
            "--target",
            "x86_64-unknown-none",
            "-Z",
            "build-std=core,alloc,compiler_builtins",
        ])
        .status()?;
    if !status.success() {
        return Err(io::Error::new(io::ErrorKind::Other, "bellows build failed"));
    }

    let bellows_path = workspace_root
        .join("target")
        .join("x86_64-unknown-none")
        .join("release")
        .join("bellows.efi");

    // 3. Create FAT32 image with EFI binaries
    let fat32_img_path = workspace_root.join("fullerene.img");
    create_fat32_image(&fat32_img_path, &bellows_path, &kernel_path)?;

    // 4. Create ISO from FAT32 image
    let iso_path = workspace_root.join("fullerene.iso");
    create_iso_from_img(&iso_path, &fat32_img_path)?;

    // 5. Prepare OVMF files
    let ovmf_dir = workspace_root.join("flasks").join("ovmf");
    let ovmf_code = ovmf_dir.join("RELEASEX64_OVMF.fd");
    let ovmf_vars = ovmf_dir.join("RELEASEX64_OVMF_VARS.fd");
    if !ovmf_code.exists() || !ovmf_vars.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "OVMF files not found in flasks/ovmf/",
        ));
    }

    // 6. Run QEMU with ISO
    let qemu_args = [
        "-drive",
        &format!(
            "if=pflash,format=raw,readonly=on,file={}",
            ovmf_code.display()
        ),
        "-drive",
        &format!("if=pflash,format=raw,file={}", ovmf_vars.display()),
        "-cdrom",
        &iso_path.display().to_string(),
        "-boot",
        "once=d",
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
