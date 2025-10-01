// fullerene/flasks/src/main.rs
use isobemak::iso::builder::{BootInfo, IsoImage, IsoImageFile, UefiBootInfo, build_iso};
use std::{env, io, path::PathBuf, process::Command};

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

    let image = IsoImage {
        files: vec![
            IsoImageFile {
                source: kernel_path.clone(),
                destination: "EFI/BOOT/KERNEL.EFI".to_string(),
            },
            IsoImageFile {
                source: bellows_path.clone(),
                destination: "EFI/BOOT/BOOTX64.EFI".to_string(),
            },
        ],
        boot_info: BootInfo {
            bios_boot: None,
            uefi_boot: Some(UefiBootInfo {
                boot_image: bellows_path.clone(),
                kernel_image: kernel_path.clone(),
                destination_in_iso: "EFI/BOOT/BOOTX64.EFI".to_string(),
            }),
        },
    };
    build_iso(&iso_path, &image, false)?;

    // --- 4. Run QEMU with the created ISO ---
    let ovmf_fd_path = workspace_root
        .join("flasks")
        .join("ovmf")
        .join("RELEASEX64_OVMF_CODE.fd");
    let ovmf_vars_fd_path = workspace_root
        .join("flasks")
        .join("ovmf")
        .join("RELEASEX64_OVMF_VARS.fd");

    let ovmf_fd_drive = format!(
        "if=pflash,format=raw,unit=0,readonly=on,file={}",
        ovmf_fd_path.display()
    );
    let ovmf_vars_fd_drive = format!(
        "if=pflash,format=raw,unit=1,file={}",
        ovmf_vars_fd_path.display()
    );

    let iso_path_str = iso_path.to_str().expect("ISO path should be valid UTF-8");

    let qemu_args = [
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
        "-cdrom",
        iso_path_str,
        "-no-reboot",
        "-d",
        "int",
        "-D",
        "qemu_log.txt",
        "-boot",
        "order=d",
        "-nodefaults",
    ];

    let qemu_status = Command::new("qemu-system-x86_64")
        .args(qemu_args)
        .status()?;

    if !qemu_status.success() {
        return Err(io::Error::other("QEMU execution failed"));
    }

    Ok(())
}
