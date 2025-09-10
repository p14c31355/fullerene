use std::process::Command;

fn main() {
    // Make ISO by bootimage
    let status = Command::new("cargo")
        .args(["bootimage"])
        .status()
        .unwrap();
    assert!(status.success());

    // QEMU run
    let status = Command::new("qemu-system-x86_64")
        .args([
            "-drive", "format=raw,file=target/x86_64-kernel/debug/bootimage-kernel.bin",
            "-serial", "stdio"
        ])
        .status()
        .unwrap();
    assert!(status.success());
}
