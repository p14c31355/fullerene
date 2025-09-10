use std::process::Command;

fn main() {
    // Build kernel
    let status = Command::new("cargo")
        .args(["build", "--package", "fullerene-kernel", "--target", "x86_64-unknown-none"])
        .status()
        .unwrap();
    assert!(status.success());

    // Make bootimage
    let status = Command::new("cargo")
        .args(["builder", "--kernel", "target/x86_64-unknown-none/debug/fullerene-kernel"])
        .status()
        .unwrap();
    assert!(status.success());

    // Run QEMU
    let status = Command::new("qemu-system-x86_64")
        .args([
            "-drive", "format=raw,file=target/bootimage.bin",
            "-serial", "stdio",
            "-bios", "/usr/share/OVMF/OVMF_CODE.fd"
        ])
        .status()
        .unwrap();
    assert!(status.success());
}
