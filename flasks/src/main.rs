// fullerene/flasks/src/main.rs
use clap::Parser;
use isobemak::{BiosBootInfo, BootInfo, IsoImage, IsoImageFile, UefiBootInfo, build_iso};
use std::{env, io, path::PathBuf, process::Command};

use env_logger;

#[derive(Parser)]
struct Args {
    /// Clone the stable version of OVMF (edk2) into flasks/ovmf/edk2
    #[arg(long)]
    clone_ovmf: bool,

    /// Run QEMU in headless mode (no GUI)
    #[arg(long)]
    headless: bool,

    /// Timeout for QEMU execution in seconds
    #[arg(long)]
    timeout: Option<u64>,
}

fn main() -> io::Result<()> {
    // Initialize env_logger - it will respect RUST_LOG environment variable for filtering
    env_logger::init();
    let args = Args::parse();
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("Failed to get workspace root")
        .to_path_buf();

    if args.clone_ovmf {
        setup_ovmf(&workspace_root)?;
        return Ok(());
    }

    run_qemu(&workspace_root, &args)?;
    Ok(())
}

fn setup_ovmf(workspace_root: &PathBuf) -> io::Result<()> {
    // 1. Clean up previous failed clone attempts if they exist
    let edk2_dir = workspace_root.join("flasks").join("ovmf").join("edk2");
    if edk2_dir.exists() {
        log::info!("Removing previous edk2 clone directory...");
        std::fs::remove_dir_all(edk2_dir)?;
    }

    // 2. Check if OVMF is installed.
    let src_code = PathBuf::from("/usr/share/OVMF/OVMF_CODE.fd");
    let src_vars = PathBuf::from("/usr/share/OVMF/OVMF_VARS.fd");
    if !src_code.exists() || !src_vars.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "OVMF binaries not found in /usr/share/OVMF/. Please install the 'ovmf' package manually (e.g., 'sudo apt-get install -y ovmf' on Debian/Ubuntu).",
        ));
    }
    log::info!("OVMF binaries found.");

    // 3. Copy .fd files to flasks/ovmf/
    let dst_code = workspace_root
        .join("flasks")
        .join("ovmf")
        .join("RELEASEX64_OVMF_CODE.fd");
    let dst_vars = workspace_root
        .join("flasks")
        .join("ovmf")
        .join("RELEASEX64_OVMF_VARS.fd");

    log::info!(
        "Copying OVMF binaries to {}...",
        workspace_root.join("flasks").join("ovmf").display()
    );
    std::fs::copy(&src_code, &dst_code)?;
    std::fs::copy(&src_vars, &dst_vars)?;

    log::info!("OVMF setup completed successfully.");
    Ok(())
}

fn create_iso_and_setup(
    workspace_root: &PathBuf,
) -> io::Result<(PathBuf, PathBuf, PathBuf, tempfile::NamedTempFile)> {
    // --- 1. Build fullerene-kernel (no_std) ---
    let status = Command::new("cargo")
        .current_dir(workspace_root)
        .args([
            "+nightly",
            "build",
            "-q",
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
    let kernel_bin_dest = workspace_root
        .join("bellows")
        .join("src")
        .join("kernel_final.bin");
    std::fs::copy(&kernel_path, &kernel_bin_dest)?;
    log::info!(
        "Copied kernel to {} (size: {})",
        kernel_bin_dest.display(),
        kernel_path.metadata()?.len()
    );

    // --- 2. Build bellows (no_std) ---
    // Force rebuild of bellows to ensure the latest kernel_final.bin is embedded
    let clean_status = Command::new("cargo")
        .current_dir(workspace_root)
        .args(["clean", "-p", "bellows"])
        .status()?;
    if !clean_status.success() {
        log::warn!("Warning: cargo clean -p bellows failed, proceeding anyway");
    }

    let bellows_path = target_dir.join("bellows.efi");
    let status = Command::new("cargo")
        .current_dir(workspace_root)
        .args([
            "+nightly",
            "build",
            "-q",
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

    // --- 3. Create ISO using isobemak ---
    let iso_path = workspace_root.join("fullerene.iso");

    // Create dummy file for BIOS boot catalog path required by BiosBootInfo struct
    let dummy_boot_catalog_path = workspace_root.join("dummy_boot_catalog.bin");
    std::fs::File::create(&dummy_boot_catalog_path)?;

    let image = IsoImage {
        volume_id: None,
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

fn run_qemu(workspace_root: &PathBuf, args: &Args) -> io::Result<()> {
    log::info!("Starting QEMU...");
    let (iso_path, ovmf_fd_path, ovmf_vars_fd_path, temp_ovmf_vars_fd) =
        create_iso_and_setup(&workspace_root)?;

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
    let mut qemu_args = vec![
        "-m".to_string(),
        "4G".to_string(),
        "-cpu".to_string(),
        "qemu64,+smap,-invtsc".to_string(),
        "-smp".to_string(),
        "1".to_string(),
        "-M".to_string(),
        "q35".to_string(),
        "-vga".to_string(),
        "std".to_string(),
    ];

    if args.headless {
        qemu_args.push("-display".to_string());
        qemu_args.push("none".to_string());
    } else {
        qemu_args.push("-display".to_string());
        qemu_args.push("gtk,gl=off,window-close=on,zoom-to-fit=on".to_string());
    }

    qemu_args.extend([
        "-serial".to_string(),
        "stdio".to_string(),
        "-accel".to_string(),
        "tcg,thread=single".to_string(),
        "-d".to_string(),
        "int,cpu_reset,guest_errors,unimp".to_string(),
        "-D".to_string(),
        "qemu_log.txt".to_string(),
        "-monitor".to_string(),
        "none".to_string(),
    ]);

    qemu_args.push("-drive".to_string());
    qemu_args.push(ovmf_fd_drive);
    qemu_args.push("-drive".to_string());
    qemu_args.push(ovmf_vars_fd_drive);
    qemu_args.push("-drive".to_string());
    qemu_args.push(format!(
        "file={},media=cdrom,if=ide,format=raw",
        iso_path_str
    ));

    qemu_args.extend([
        "-no-reboot".to_string(),
        "-no-shutdown".to_string(),
        "-device".to_string(),
        "isa-debug-exit,iobase=0xf4,iosize=0x04".to_string(),
        "-rtc".to_string(),
        "base=utc".to_string(),
        "-boot".to_string(),
        "menu=on,order=d".to_string(),
        "-nodefaults".to_string(),
    ]);

    qemu_cmd.args(&qemu_args);

    // Keep the temporary file alive until QEMU exits
    let _temp_ovmf_vars_fd_holder = temp_ovmf_vars_fd;
    // LD_PRELOAD is a workaround for specific QEMU/libpthread versions.
    // It can be overridden by setting the FULLERENE_QEMU_LD_PRELOAD environment variable.
    let ld_preload_path = env::var("FULLERENE_QEMU_LD_PRELOAD").unwrap_or_else(|_| {
        flasks::find_libpthread().expect("libpthread.so.0 not found in common locations")
    });
    qemu_cmd.env("LD_PRELOAD", ld_preload_path);

    let mut child = qemu_cmd.spawn()?;

    if let Some(timeout_secs) = args.timeout {
        let timeout_duration = std::time::Duration::from_secs(timeout_secs);
        let timeout_handle = std::thread::spawn(move || {
            std::thread::sleep(timeout_duration);
        });

        // We need to wait for either the child to exit or the timeout thread to finish
        // Since we can't easily "select" on a process, we'll poll the child
        loop {
            match child.try_wait()? {
                Some(status) => {
                    if !status.success() {
                        return Err(io::Error::other("QEMU execution failed"));
                    }
                    return Ok(());
                }
                None => {
                    if timeout_handle.is_finished() {
                        log::warn!(
                            "QEMU timed out after {} seconds. Killing process...",
                            timeout_secs
                        );
                        child.kill()?;
                        return Err(io::Error::other("QEMU execution timed out"));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
    } else {
        let qemu_status = child.wait()?;
        if !qemu_status.success() {
            return Err(io::Error::other("QEMU execution failed"));
        }
    }

    Ok(())
}
