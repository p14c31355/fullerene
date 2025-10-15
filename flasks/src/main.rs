// fullerene/flasks/src/main.rs
use clap::Parser;
use isobemak::{BiosBootInfo, BootInfo, IsoImage, IsoImageFile, UefiBootInfo, build_iso};
use std::{env, io, path::PathBuf, process::Command};

#[derive(Parser)]
struct Args {
    /// Use QEMU instead of VirtualBox for virtualization
    #[arg(long)]
    qemu: bool,
}


fn main() -> io::Result<()> {
    let args = Args::parse();

    if args.qemu {
        run_qemu()?;
    } else {
        run_virtualbox()?;
    }
    Ok(())
}

fn create_iso_and_setup(workspace_root: &PathBuf) -> io::Result<(PathBuf, PathBuf, PathBuf, tempfile::NamedTempFile)> {
    // --- 1. Build fullerene-kernel (no_std) ---
    let status = Command::new("cargo")
        .current_dir(workspace_root)
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
    // Copy kernel to bellows/src for embedding
    std::fs::copy(&kernel_path, "bellows/src/kernel.bin")?;

    // --- 2. Build bellows (no_std) ---
    // For BIOS mode, skip bellows
    let bellows_path = target_dir.join("bellows.efi");
    if bellows_path.exists() {
        // Use existing
    } else {
        let status = Command::new("cargo")
            .current_dir(workspace_root)
            .args([
                "+nightly",
                "build",
                "-Zbuild-std=core,alloc",
                "--features",
                "debug_loader",
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
                boot_catalog: dummy_boot_catalog_path.clone(),
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

    Ok((iso_path, ovmf_fd_path, ovmf_vars_fd_path, temp_ovmf_vars_fd))
}

fn run_virtualbox() -> io::Result<()> {
    println!("Starting VirtualBox...");
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("Failed to get workspace root")
        .to_path_buf();

    let (iso_path, _ovmf_fd_path, _ovmf_vars_fd_path, _dummy) = create_iso_and_setup(&workspace_root)?;

    // Start VirtualBox VM with the ISO
    let mut vbox_cmd = Command::new("VBoxManage");
    vbox_cmd.args([
        "startvm",
        "fullerene-vm", // Assume VM name, could be configurable later
        "--type",
        "gui",
    ]);

    // Add ISO as storage attachment
    let mut attach_cmd = Command::new("VBoxManage");
    let iso_path_str = iso_path.to_str().expect("ISO path should be valid UTF-8");
    attach_cmd.args([
        "storageattach",
        "fullerene-vm",
        "--storagectl",
        "IDE Controller", // Assume default controller name
        "--port",
        "0",
        "--device",
        "0",
        "--type",
        "dvddrive",
        "--medium",
        iso_path_str,
    ]);

    // Execute attach command first
    match attach_cmd.status() {
        Ok(status) if status.success() => {},
        _ => return Err(io::Error::other("Failed to attach ISO to VirtualBox VM")),
    }

    // Then start VM
    let vbox_status = vbox_cmd.status()?;
    if !vbox_status.success() {
        return Err(io::Error::other("VirtualBox VM startup failed"));
    }

    Ok(())
}

fn run_qemu() -> io::Result<()> {
    println!("Starting QEMU...");
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("Failed to get workspace root")
        .to_path_buf();

    let (_iso_path, ovmf_fd_path, ovmf_vars_fd_path, temp_ovmf_vars_fd) = create_iso_and_setup(&workspace_root)?;
    let iso_path = workspace_root.join("fullerene.iso");

    // --- 4. Run QEMU with the created ISO ---

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
        "8G",
        "-cpu",
        "qemu64,+smap,-invtsc",
        "-smp",
        "1",
        "-M",
        "q35",
        "-vga",
        "cirrus",
        "-display",
        "gtk,gl=on,window-close=on,zoom-to-fit=on",
        "-serial",
        "stdio",
        "-accel",
        "tcg,thread=single",
        "-d",
        "guest_errors,unimp",
        "-D",
        "qemu_log.txt",
        "-monitor",
        "none",
        "-drive",
        &ovmf_fd_drive,
        "-drive",
        &ovmf_vars_fd_drive,
        "-drive",
        &format!("file={},media=cdrom,if=ide,format=raw", iso_path_str),
        "-no-reboot",
        "-no-shutdown",
        "-device",
        "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-boot",
        "menu=on,order=d",
        "-nodefaults",
    ]);
    // Keep the temporary file alive until QEMU exits
    let _temp_ovmf_vars_fd_holder = temp_ovmf_vars_fd;
    // LD_PRELOAD is a workaround for specific QEMU/libpthread versions.
    // It can be overridden by setting the FULLERENE_QEMU_LD_PRELOAD environment variable.
    let ld_preload_path = env::var("FULLERENE_QEMU_LD_PRELOAD").unwrap_or_else(|_| {
        flasks::find_libpthread().expect("libpthread.so.0 not found in common locations")
    });
    qemu_cmd.env("LD_PRELOAD", ld_preload_path);
    let qemu_status = qemu_cmd.status()?;

    if !qemu_status.success() {
        return Err(io::Error::other("QEMU execution failed"));
    }

    Ok(())
}
