// fullerene/flasks/src/main.rs
use std::{process::Command, fs, path::Path};

fn run(cmd: &mut Command) {
    println!("$ {:?}", cmd);
    let status = cmd.status().expect("failed to run command");
    assert!(status.success(), "command failed");
}

fn main() {
    // 1. Build bellows (UEFI bootloader)
    run(Command::new("cargo").args([
        "build",
        "--package", "bellows",
        "--release",
        "--target", "x86_64-uefi.json",
        "-Z", "build-std=core,alloc,compiler_builtins",
    ]));

    // 2. Build the kernel
    run(Command::new("cargo").args([
        "build",
        "--package", "fullerene-kernel",
        "--release",
        "--target", "x86_64-uefi.json",
        "-Z", "build-std=core,alloc,compiler_builtins",
    ]));

    // 3. Create FAT image for UEFI boot
    let esp_path = Path::new("esp.img");
    if esp_path.exists() {
        fs::remove_file(esp_path).unwrap();
    }

    run(Command::new("dd").args([
        "if=/dev/zero", "of=esp.img", "bs=1M", "count=64"
    ]));
    run(Command::new("mkfs.vfat").args(["-F", "32", "esp.img"]));

    // 4. Use mtools (no sudo needed)
    run(Command::new("mmd").args(["-i", "esp.img", "::/EFI"]));
    run(Command::new("mmd").args(["-i", "esp.img", "::/EFI/BOOT"]));

    let bellows_efi = "target/x86_64-uefi/release/bellows";
    run(Command::new("mcopy").args([
        "-i", "esp.img", bellows_efi, "::/EFI/BOOT/BOOTX64.EFI"
    ]));

    let kernel_efi = "target/x86_64-uefi/release/fullerene-kernel";
    run(Command::new("mcopy").args([
        "-i", "esp.img", kernel_efi, "::/kernel.efi"
    ]));

    // 5. Run QEMU with OVMF firmware
    run(Command::new("qemu-system-x86_64").args([
        "-drive", "if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE.fd",
        "-drive", "format=raw,file=esp.img",
        "-serial", "stdio",
    ]));
}
