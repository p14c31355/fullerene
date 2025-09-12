// fullerene/flasks/src/main.rs
use std::{
    fs,
    io::{self},
    path::Path,
    process::Command,
};

fn main() -> io::Result<()> {
    // 1. Build fullerene-kernel
    let status = Command::new("cargo")
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
            "--features",
            "",
        ])
        .status()?;
    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "fullerene-kernel build failed",
        ));
    }

    // 2. Build bellows
    let status = Command::new("cargo")
        .args([
            "build",
            "--package",
            "bellows",
            "--release",
            "--target",
            "x86_64-uefi.json",
            "-Z",
            "build-std=core,alloc,compiler_builtins",
        ])
        .status()?;
    if !status.success() {
        return Err(io::Error::new(io::ErrorKind::Other, "bellows build failed"));
    }

    // 3. Prepare ISO directory structure
    let iso_dir = Path::new("iso");
    if iso_dir.exists() {
        fs::remove_dir_all(iso_dir)?;
    }
    fs::create_dir_all(iso_dir.join("EFI").join("BOOT"))?;

    // 4. Copy EFI binaries to the ISO directory
    let bellows_efi_src = Path::new("target/x86_64-uefi/release/bellows");
    let bellows_efi_dest = iso_dir.join("EFI").join("BOOT").join("BOOTX64.EFI");
    fs::copy(bellows_efi_src, bellows_efi_dest)?;

    let kernel_efi_src = Path::new("target/x86_64-uefi/release/fullerene-kernel");
    let kernel_efi_dest = iso_dir.join("kernel.efi");
    fs::copy(kernel_efi_src, kernel_efi_dest)?;

    // 5. Create UEFI bootable ISO image
    let iso_path = Path::new("esp.iso");
    if iso_path.exists() {
        fs::remove_file(iso_path)?;
    }

    // Find the correct path for isohdpfx.bin, searching the entire file system
    let isohdpfx_output = Command::new("sh")
        .arg("-c")
        .arg("find / -name isohdpfx.bin 2>/dev/null | head -n 1")
        .output()?;
    
    let isohdpfx_path = String::from_utf8(isohdpfx_output.stdout)
        .unwrap()
        .trim()
        .to_string();

    if isohdpfx_path.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "isohdpfx.bin not found. Please ensure the syslinux-utils or xorriso package is installed and accessible.",
        ));
    }

    let iso_creation_status = Command::new("xorriso")
        .args([
            "-as",
            "mkisofs",
            "-o",
            iso_path.to_str().unwrap(),
            "-e",
            "EFI/BOOT/BOOTX64.EFI",
            "-no-emul-boot",
            "-isohybrid-gpt-basdat",
            "-isohybrid-mbr",
            &isohdpfx_path,
            iso_dir.to_str().unwrap(),
        ])
        .status()?;

    if !iso_creation_status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "ISO creation failed with xorriso",
        ));
    }

    // 6. Copy OVMF_VARS.fd if missing
    let ovmf_code = "/usr/share/OVMF/OVMF_CODE_4M.fd";
    let ovmf_vars = "./OVMF_VARS.fd";
    if !Path::new(ovmf_vars).exists() {
        fs::copy("/usr/share/OVMF/OVMF_VARS_4M.fd", ovmf_vars)?;
    }

    // 7. Run QEMU with the ISO image
    let qemu_args = [
        "-drive",
        &format!("if=pflash,format=raw,readonly=on,file={}", ovmf_code),
        "-drive",
        &format!("if=pflash,format=raw,file={}", ovmf_vars),
        "-drive",
        "file=esp.iso,media=cdrom",
        "-m",
        "512M",
        "-cpu",
        "qemu64,+smap",
        "-serial",
        "stdio",
        "-boot",
        "order=d", // 'd' for CD-ROM
    ];
    println!("Running QEMU with args: {:?}", qemu_args);
    let qemu_status = Command::new("qemu-system-x86_64")
        .args(&qemu_args)
        .status()?;
    assert!(qemu_status.success());

    Ok(())
}