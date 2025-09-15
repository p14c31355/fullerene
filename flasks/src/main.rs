// fullerene/flasks/src/main.rs
// use isobemak::create_disk_and_iso; // Removed as create_disk_and_iso is no longer available

use std::{env, fs, io, path::PathBuf, process::Command}; // fs::File is no longer directly used

/// Build kernel and bellows, create UEFI bootable ISO with xorriso, and run QEMU
fn main() -> io::Result<()> {
    // 0. Workspace root dynamically
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("Failed to get workspace root")
        .to_path_buf();

    // 1. Build fullerene-kernel
    let status = Command::new("cargo")
        .current_dir(&workspace_root)
        .env("RUST_TARGET_PATH", &workspace_root)
        .env("RUSTFLAGS", &format!("-C link-arg=-T{}", workspace_root.join("linker.ld").display()))
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
            "--target-dir",
            "target",
            "--message-format=json",
        ])
        .output()?;
    if !status.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "fullerene-kernel build failed",
        ));
    }
    let kernel_path = parse_cargo_json_output(&status.stdout, "fullerene-kernel")?;
    eprintln!("Kernel EFI path: {}", kernel_path.display());

    // 2. Build bellows
    let status = Command::new("cargo")
        .current_dir(&workspace_root)
        .env("RUST_TARGET_PATH", &workspace_root)
        .env("RUSTFLAGS", &format!("-C link-arg=-T{}", workspace_root.join("linker.ld").display()))
        .args([
            "build",
            "--package",
            "bellows",
            "--release",
            "--target",
            "x86_64-uefi.json",
            "-Z",
            "build-std=core,alloc,compiler_builtins",
            "--target-dir",
            "target",
            "--message-format=json",
        ])
        .output()?;
    if !status.status.success() {
        return Err(io::Error::new(io::ErrorKind::Other, "bellows build failed"));
    }
    let bellows_path = parse_cargo_json_output(&status.stdout, "bellows")?;
    eprintln!("Bellows EFI path: {}", bellows_path.display());

    // 3. Paths to binaries
    // The paths are now obtained from the JSON output of cargo build
    // The previous hardcoded paths and existence checks are removed as parse_cargo_json_output handles this.

    eprintln!("Kernel EFI path: {}", kernel_path.display());
    eprintln!("Bellows EFI path: {}", bellows_path.display());

    // 4. ISO path
    let iso_path = workspace_root.join("fullerene.iso");

    println!("ISO Path: {}", iso_path.display());
    println!("ISO Exists before QEMU: {}", iso_path.exists());

    // 5. Create ISO image containing EFI binaries directly
    if let Err(e) = isobemak::create_iso(
        &iso_path,
        &bellows_path,
        &kernel_path,
    ) {
        eprintln!("Error from create_iso: {:?}", e);
        return Err(e);
    }

    println!("ISO Exists after creation: {}", iso_path.exists());

    // 6. Prepare OVMF paths
    let ovmf_dir = workspace_root.join("flasks").join("ovmf");
    let ovmf_code = ovmf_dir.join("RELEASEX64_OVMF.fd");
    let ovmf_vars = ovmf_dir.join("RELEASEX64_OVMF_VARS.fd");

    if !ovmf_code.exists() || !ovmf_vars.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "OVMF files not found in flasks/ovmf/",
        ));
    }

    // 7. Run QEMU with FAT32 image as direct boot
    let qemu_args = [
        "-drive",
        &format!(
            "if=pflash,format=raw,readonly=on,file={}",
            ovmf_code.display()
        ),
        "-drive",
        &format!("if=pflash,format=raw,file={}", ovmf_vars.display()),
        "-cdrom",
        &format!("{}", iso_path.display()), // Boot from ISO
        "-boot",
        "once=d",
        "-m",
        "512M",
        "-cpu",
        "qemu64,+smap",
        "-nographic",
        "-serial",
        "mon:stdio",
        // "-vga",
        // "std",
        
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

/// Parses the JSON output from `cargo build --message-format=json` to find the path of the
/// compiled EFI binary for a given package.
fn parse_cargo_json_output(output: &[u8], package_name: &str) -> io::Result<PathBuf> {
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Target {
        name: String,
    }

    #[derive(Deserialize)]
    struct Message {
        reason: String,
        target: Option<Target>,
        filenames: Option<Vec<String>>,
    }

    for line in output.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }

        if let Ok(msg) = serde_json::from_slice::<Message>(line) {
            if msg.reason == "compiler-artifact" {
                if let (Some(target), Some(filenames)) = (msg.target, msg.filenames) {
                    if target.name == package_name {
                        if let Some(filename) = filenames.iter().find(|f| f.ends_with(".efi")) {
                            return Ok(PathBuf::from(filename));
                        }
                    }
                }
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("EFI file for package '{}' not found in cargo build output", package_name),
    ))
}
