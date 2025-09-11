use std::io::Seek;
// fullerene/flasks/src/main.rs
use std::{fs, io::Write, path::Path};
use fatfs::{FileSystem, FormatVolumeOptions, FsOptions};
use std::fs::File;
use std::process::Command;

fn copy_to_fat<P: AsRef<Path>>(fs: &fatfs::FileSystem<File>, src: P, dest: &str) -> std::io::Result<()> {
    let mut f = fs.root_dir().create_file(dest)?;
    let data = fs::read(src)?;
    f.write_all(&data)?;
    Ok(())
}

fn main() -> std::io::Result<()> {
    // 1. Build bellows (UEFI bootloader)
    let status = Command::new("cargo").args([
        "build",
        "--package", "bellows",
        "--release",
        "--target", "x86_64-uefi.json",
        "-Z", "build-std=core,alloc,compiler_builtins",
    ]).status()?;
    assert!(status.success());

    // 2. Build the kernel
    let status = Command::new("cargo").args([
        "build",
        "--package", "fullerene-kernel",
        "--release",
        "--target", "x86_64-uefi.json",
        "-Z", "build-std=core,alloc,compiler_builtins",
    ]).status()?;
    assert!(status.success());

    // 3. Create FAT32 image
    let esp_path = Path::new("esp.img");
    if esp_path.exists() {
        fs::remove_file(esp_path)?;
    }

    // Create an empty file
    let mut f = File::create(esp_path)?;
    f.set_len(64 * 1024 * 1024)?; // 64 MB

    // Format FAT32
    let opts = FormatVolumeOptions::new().volume_label(*b" FULLERENE ");
    fatfs::format_volume(&mut f, opts)?;

    // Mount FAT filesystem
    f.seek(std::io::SeekFrom::Start(0))?;
    let fs = FileSystem::new(f, FsOptions::new())?;

    // 4. Create EFI directories
    fs.root_dir().create_dir("EFI")?;
    fs.root_dir().open_dir("EFI")?.create_dir("BOOT")?;

    // 5. Copy EFI files
    copy_to_fat(&fs, "target/x86_64-uefi/release/bellows", "EFI/BOOT/BOOTX64.EFI")?;
    copy_to_fat(&fs, "target/x86_64-uefi/release/fullerene-kernel", "kernel.efi")?;

    drop(fs);
    
    // 6. Run QEMU with OVMF firmware
    let ovmf_path = "/usr/share/OVMF/OVMF_CODE_4M.fd";
    if !Path::new(ovmf_path).exists() {
        panic!("OVMF firmware not found at {}", ovmf_path);
    }

    Command::new("qemu-system-x86_64")
        .args([
            "-drive", &format!("if=pflash,format=raw,readonly=on,file={}", ovmf_path),
            "-drive", "format=raw,file=esp.img",
            "-serial", "stdio",
        ])
        .status()?;

    Ok(())
}
