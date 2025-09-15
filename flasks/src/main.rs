// fullerene/flasks/src/main.rs
use serde::Deserialize;
use std::{env, io, path::PathBuf, process::Command};

/// Build kernel and bellows, create UEFI bootable image, and run QEMU
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
    eprintln!("Kernel EFI path: {}", kernel_path.display());
    eprintln!("Bellows EFI path: {}", bellows_path.display());

    // 4. FAT32 image path
    let fat32_path = workspace_root.join("fullerene.img");
    println!("FAT32 Image Path: {}", fat32_path.display());

    // 5. Create FAT32 image containing EFI binaries
    if let Err(e) = isobemak::create_fat32_image(
        &fat32_path,
        &bellows_path,
        &kernel_path,
    ) {
        eprintln!("Error from create_fat32_image: {:?}", e);
        return Err(e);
    }

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

    // 7. Run QEMU with FAT32 image as a drive
    let qemu_args = [
        "-drive",
        &format!(
            "if=pflash,format=raw,readonly=on,file={}",
            ovmf_code.display()
        ),
        "-drive",
        &format!("if=pflash,format=raw,file={}", ovmf_vars.display()),
        "-drive",
        &format!("format=raw,file={}", fat32_path.display()), // Boot from FAT32 image
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
