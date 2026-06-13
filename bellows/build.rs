//! Build script for bellows.
//!
//! Copies the kernel binary into `OUT_DIR` so it can be embedded
//! via `include_bytes!` without polluting the source tree.
//!
//! The caller (flasks) sets `KERNEL_BIN_PATH` to the absolute path
//! of the kernel EFI binary before invoking `cargo build`.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let kernel_path = env::var("KERNEL_BIN_PATH").unwrap_or_else(|_| {
        // Fallback for local development: use the file in src/
        let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
        manifest_dir
            .join("src")
            .join("kernel.bin")
            .to_string_lossy()
            .to_string()
    });

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let dest = out_dir.join("kernel.bin");

    println!("cargo:rerun-if-env-changed=KERNEL_BIN_PATH");
    println!("cargo:rerun-if-changed={}", kernel_path);

    fs::copy(&kernel_path, &dest).unwrap_or_else(|e| {
        panic!(
            "Failed to copy kernel binary from '{}' to '{}': {}",
            kernel_path,
            dest.display(),
            e
        );
    });

    println!("cargo:warning=Embedding kernel from {}", kernel_path);
}