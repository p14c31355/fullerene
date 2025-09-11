// fullerene/flasks/src/main.rs
use std::{fs, io::Write, path::{Path, PathBuf}, process::Command};
use fatfs::{FileSystem, FormatVolumeOptions, FsOptions};
use std::fs::File;

/// Copy a file into the FAT filesystem, creating directories as needed
fn copy_to_fat<P: AsRef<Path>>(fs: &FileSystem<File>, src: P, dest: &str) -> std::io::Result<()> {
    let dest_path = Path::new(dest);
    let mut dir = fs.root_dir();

    // Create intermediate directories
    for component in dest_path.parent().into_iter().flat_map(|p| p.iter()) {
        let name = component.to_str().unwrap();
        if dir.iter().filter_map(|e| e.ok()).find(|e| e.file_name() == name).is_none() {
            dir = dir.create_dir(name)?;
        } else {
            dir = dir.open_dir(name)?;
        }
    }

    // Create and write file
    let mut f = dir.create_file(dest_path.file_name().unwrap().to_str().unwrap())?;
    let data = fs::read(src)?;
    f.write_all(&data)?;
    Ok(())
}

fn main() -> std::io::Result<()> {
    // 1. Build fullerene-kernel
    let status = Command::new("cargo")
        .args([
            "build",
            "--package", "fullerene-kernel",
            "--release",
            "--target", "x86_64-uefi.json",
            "-Z", "build-std=core,alloc,compiler_builtins",
        ])
        .status()?;
    assert!(status.success());

    // 2. Build bellows (UEFI bootloader)
    let status = Command::new("cargo")
        .args([
            "build",
            "--package", "bellows",
            "--release",
            "--target", "x86_64-uefi.json",
            "-Z", "build-std=core,alloc,compiler_builtins",
        ])
        .status()?;
    assert!(status.success());

    // 3. Create FAT32 image
    let esp_path = Path::new("esp.img");
    if esp_path.exists() {
        fs::remove_file(esp_path)?;
    }

    {
        let mut f = File::create(esp_path)?;
        f.set_len(64 * 1024 * 1024)?; // 64 MB
        let opts = FormatVolumeOptions::new().volume_label(*b" FULLERENE ");
        fatfs::format_volume(&mut f, opts)?;
    }

    // 4. Open FAT filesystem
    let f = File::options().read(true).write(true).open(esp_path)?;
    let fs = FileSystem::new(f, FsOptions::new())?;

    // 5. Create EFI directories
    fs.root_dir().create_dir("EFI")?;
    fs.root_dir().open_dir("EFI")?.create_dir("BOOT")?;

    // 6. Copy EFI files
    let bellows_efi = "target/x86_64-uefi/release/bellows";
    let kernel_efi = "target/x86_64-uefi/release/fullerene-kernel";

    if !Path::new(bellows_efi).exists() {
        panic!("bellows EFI not found: {}", bellows_efi);
    }
    if !Path::new(kernel_efi).exists() {
        panic!("fullerene-kernel EFI not found: {}", kernel_efi);
    }

    copy_to_fat(&fs, bellows_efi, "EFI/BOOT/BOOTX64.EFI")?;
    copy_to_fat(&fs, kernel_efi, "kernel.efi")?;

    drop(fs); // flush filesystem

    // 7. Run QEMU with OVMF
    let ovmf_path = "/usr/share/OVMF/OVMF_CODE_4M.fd";
    if !Path::new(ovmf_path).exists() {
        panic!("OVMF firmware not found at {}", ovmf_path);
    }

    let qemu_args = [
    "-drive", &format!("if=pflash,format=raw,readonly=on,file={}", ovmf_path),
    "-drive", &format!("format=raw,file={},if=ide", "esp.img"),
    "-m", "512M",
    "-cpu", "qemu64",
    "-serial", "stdio",
];


    println!("Running QEMU with args: {:?}", qemu_args);

    let qemu_status = Command::new("qemu-system-x86_64")
        .args(qemu_args)
        .status()?;

    assert!(qemu_status.success());

    Ok(())
}
