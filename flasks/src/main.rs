// fullerene/flasks/src/main.rs
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};
use fatfs::{FileSystem, FormatVolumeOptions, FsOptions};
use std::fs::File;

/// Copy a file into the FAT filesystem, creating directories as needed
fn copy_to_fat<P: AsRef<Path>>(
    fs: &FileSystem<File>,
    src: P,
    dest: &str,
) -> std::io::Result<()> {
    let dest_path = Path::new(dest);
    let mut dir = fs.root_dir();

    // Create intermediate directories
    if let Some(parent) = dest_path.parent() {
        for component in parent.iter() {
            let name = component.to_str().unwrap();
            let found = dir
                .iter()
                .filter_map(|e| e.ok())
                .any(|e| e.file_name().eq_ignore_ascii_case(name));
            dir = if found {
                dir.open_dir(name)?
            } else {
                dir.create_dir(name)?
            };
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
            "--package",
            "fullerene-kernel",
            "--release",
            "--target",
            "x86_64-uefi.json",
            "-Z",
            "build-std=core,alloc,compiler_builtins",
        ])
        .status()?;
    assert!(status.success());

    // 2. Build bellows (UEFI bootloader)
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
    assert!(status.success());

    // 3. Create a blank 64MB disk image
    let esp_path = Path::new("esp.img");
    if esp_path.exists() {
        fs::remove_file(esp_path)?;
    }
    Command::new("qemu-img")
        .args(["create", "esp.img", "64M"])
        .status()?
        .success();

    // 4. Partition with GPT and create EFI System Partition (ef00)
    Command::new("sgdisk")
        .args(["-n", "1:2048:", "-t", "1:ef00", "esp.img"])
        .status()?
        .success();

    // 5. Setup loop device and format as FAT32
    Command::new("sudo")
        .args(["losetup", "-Pf", "esp.img"])
        .status()?;
    let loop_dev = "/dev/loop0p1"; // FIXME: detect dynamically
    Command::new("sudo")
        .args(["mkfs.fat", "-F32", loop_dev])
        .status()?;

    // 6. Mount partition and copy files
    Command::new("sudo")
        .args(["mount", loop_dev, "/mnt"])
        .status()?;

    let bellows_efi = "target/x86_64-uefi/release/bellows";
    let kernel_efi = "target/x86_64-uefi/release/fullerene-kernel";

    fs::create_dir_all("/mnt/EFI/BOOT")?;
    fs::copy(bellows_efi, "/mnt/EFI/BOOT/BOOTX64.EFI")?;
    fs::copy(kernel_efi, "/mnt/kernel.efi")?;

    Command::new("sudo").args(["umount", "/mnt"]).status()?;
    Command::new("sudo").args(["losetup", "-d", "/dev/loop0"]).status()?;

    // 7. Run QEMU with OVMF
    let ovmf_code = "/usr/share/OVMF/OVMF_CODE_4M.fd";
    let ovmf_vars = "./OVMF_VARS.fd";
    if !Path::new(ovmf_vars).exists() {
        fs::copy("/usr/share/OVMF/OVMF_VARS_4M.fd", ovmf_vars)?;
    }

    let qemu_args = [
        "-drive",
        &format!("if=pflash,format=raw,readonly=on,file={}", ovmf_code),
        "-drive",
        &format!("if=pflash,format=raw,file={}", ovmf_vars),
        "-drive",
        "file=esp.img,format=raw,if=ide",
        "-m",
        "512M",
        "-cpu",
        "qemu64,+smap",
        "-serial",
        "stdio",
        "-boot",
        "order=c",
    ];

    println!("Running QEMU with args: {:?}", qemu_args);

    let qemu_status = Command::new("qemu-system-x86_64")
        .args(&qemu_args)
        .status()?;
    assert!(qemu_status.success());

    Ok(())
}
