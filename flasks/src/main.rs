// fullerene/flasks/src/main.rs
use std::process::Command;

fn main() {
    // 1. Build the kernel and create the bootable disk image using cargo bootimage
    let status = Command::new("cargo")
    .args([
        "bootimage",
        "--package",
        "fullerene-kernel",
        "--target",
        "x86_64-unknown-none",
        "-Z",
        "build-std=core,compiler_builtins", // !
    ])
    .status()
    .expect("Failed to execute cargo bootimage");
    // Check if the command was successful
    assert!(status.success(), "Failed to build the bootable image.");

    // 2. Run QEMU with the generated bootable image
    // Note: The bootimage crate places the output in a specific path.
    let status = Command::new("qemu-system-x86_64")
        .args([
            "-drive",
            "format=raw,file=target/x86_64-unknown-none/debug/bootimage-fullerene-kernel.bin",
            "-serial",
            "stdio",
            "-bios",
            "/usr/share/OVMF/OVMF_CODE.fd",
        ])
        .status()
        .expect("Failed to execute QEMU");
    
    // Check if the QEMU command was successful
    assert!(status.success(), "QEMU command failed.");
}