// fullerene/flasks/src/main.rs
use std::process::Command;

fn main() {
    // 1. Build the kernel with build-std
    let status = Command::new("cargo")
        .args([
            "build",
            "--package", "fullerene-kernel",
            "--target", "x86_64-unknown-none",
            "-Z", "build-std=core,compiler_builtins",
        ])
        .status()
        .expect("Failed to build kernel");
    assert!(status.success(), "Kernel build failed.");

    // 2. Run QEMU directly with the ELF output
    let status = Command::new("qemu-system-x86_64")
        .args([
            "-kernel",
            "target/x86_64-unknown-none/debug/fullerene-kernel",
            "-serial", "stdio",
        ])
        .status()
        .expect("Failed to execute QEMU");
    assert!(status.success(), "QEMU command failed.");
}
