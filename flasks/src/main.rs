// fullerene/flasks/src/main.rs
use isobemak::{BiosBootInfo, BootInfo, IsoImage, IsoImageFile, UefiBootInfo, build_iso};
use std::{
    env, io,
    path::{Path, PathBuf},
    process::Command,
};

fn main() -> io::Result<()> {
    println!("Starting flasks application...");
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
    // For BIOS mode, skip bellows
    let bellows_path = target_dir.join("bellows.efi");
    if bellows_path.exists() {
        // Use existing
    } else {
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
    }

    // --- 3. Create ISO using isobemak ---
    let iso_path = workspace_root.join("fullerene.iso");

    // Create dummy file for BIOS boot catalog path required by BiosBootInfo struct
    let dummy_boot_catalog_path = workspace_root.join("dummy_boot_catalog.bin");
    std::fs::File::create(&dummy_boot_catalog_path)?;

    let image = IsoImage {
        files: vec![
            IsoImageFile {
                source: kernel_path.clone(),
                destination: "EFI\\BOOT\\KERNEL.EFI".to_string(),
            },
            IsoImageFile {
                source: bellows_path.clone(),
                destination: "EFI\\BOOT\\BOOTX64.EFI".to_string(),
            },
        ],
        boot_info: BootInfo {
            bios_boot: Some(BiosBootInfo {
                boot_catalog: dummy_boot_catalog_path,
                boot_image: bellows_path.clone(),
                destination_in_iso: "EFI\\BOOT\\BOOTX64.EFI".to_string(),
            }),
            uefi_boot: Some(UefiBootInfo {
                boot_image: bellows_path.clone(),
                kernel_image: kernel_path.clone(),
                destination_in_iso: "EFI\\BOOT\\BOOTX64.EFI".to_string(),
            }),
        },
    };
    build_iso(&iso_path, &image, true)?; // Set to true for isohybrid UEFI boot

    // --- 4. Run QEMU with the created ISO ---
    let ovmf_fd_path = workspace_root
        .join("flasks")
        .join("ovmf")
        .join("RELEASEX64_OVMF_CODE.fd");
    let ovmf_vars_fd_original_path = workspace_root
        .join("flasks")
        .join("ovmf")
        .join("RELEASEX64_OVMF_VARS.fd");

    // Create a temporary file for OVMF_VARS.fd to ensure a clean state each run
    let mut temp_ovmf_vars_fd = tempfile::NamedTempFile::new()?;
    std::io::copy(
        &mut std::fs::File::open(&ovmf_vars_fd_original_path)?,
        temp_ovmf_vars_fd.as_file_mut(),
    )?;
    let ovmf_vars_fd_path = temp_ovmf_vars_fd.path().to_path_buf();

    let ovmf_fd_drive = format!(
        "if=pflash,format=raw,unit=0,readonly=on,file={}",
        ovmf_fd_path.display()
    );
    let ovmf_vars_fd_drive = format!(
        "if=pflash,format=raw,unit=1,file={}",
        ovmf_vars_fd_path.display()
    );

    let iso_path_str = iso_path.to_str().expect("ISO path should be valid UTF-8");

    let mut qemu_cmd = Command::new("qemu-system-x86_64");
    qemu_cmd.args([
        "-m",
        "4G",
        "-cpu",
        "qemu64,+smap,-invtsc",
        "-smp",
        "1",
        "-machine",
        "q35,smm=off",
        "-vga",
        "virtio",
        "-serial",
        "stdio",
        "-monitor",
        "telnet:localhost:1234,server,nowait",
        "-accel",
        "tcg,thread=single",
        "-d",
        "guest_errors",
        "-global",
        "driver=or6.efi-setup,property=Enable,value=0",
        "-drive",
        &ovmf_fd_drive,
        "-drive",
        &ovmf_vars_fd_drive,
        "-drive",
        &format!("file={},if=virtio,format=raw", iso_path_str),
        "-no-reboot",
        "-no-shutdown",
        "-boot",
        "strict=on,order=d",
        "-nodefaults",
    ]);
    // Keep the temporary file alive until QEMU exits
    let _temp_ovmf_vars_fd_holder = temp_ovmf_vars_fd;
    // LD_PRELOAD is a workaround for specific QEMU/libpthread versions.
    // It can be overridden by setting the FULLERENE_QEMU_LD_PRELOAD environment variable.
    let ld_preload_path =
        env::var("FULLERENE_QEMU_LD_PRELOAD").unwrap_or_else(|_| find_libpthread());
    qemu_cmd.env("LD_PRELOAD", ld_preload_path);
    let qemu_status = qemu_cmd.status()?;

    if !qemu_status.success() {
        return Err(io::Error::other("QEMU execution failed"));
    }

    Ok(())
}

/// Finds the path to `libpthread.so.0` in common locations.
///
/// This function is a workaround for the `LD_PRELOAD` issue with QEMU on some systems.
/// It checks a list of common paths for the library and returns the first one that exists.
/// If the library is not found, it returns a default path.
fn find_libpthread() -> String {
    const COMMON_PATHS: &[&str] = &[
        "/lib/x86_64-linux-gnu/libpthread.so.0", // Debian/Ubuntu
        "/usr/lib64/libpthread.so.0",            // Fedora/CentOS
        "/usr/lib/libpthread.so.0",              // Arch/Other
    ];

    for path in COMMON_PATHS {
        if Path::new(path).exists() {
            return path.to_string();
        }
    }

    // Fallback to the original default if not found, with a warning.
    eprintln!(
        "warning: libpthread.so.0 not found in common paths, falling back to default. Set FULLERENE_QEMU_LD_PRELOAD to override if this fails."
    );
    "/lib/x86_64-linux-gnu/libpthread.so.0".to_string()
}
