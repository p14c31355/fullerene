//! Build script for fullerene-kernel.
//!
//! Copies asset files into `OUT_DIR` so they can be embedded
//! via `include_bytes!` without polluting the source tree.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let assets = ["badapple.rle", "badapple.pcm"];

    for asset in &assets {
        let src = manifest_dir.join("assets").join(asset);
        let dst = out_dir.join(asset);

        println!("cargo:rerun-if-changed={}", src.display());

        fs::copy(&src, &dst).unwrap_or_else(|e| {
            panic!(
                "Failed to copy asset '{}' to '{}': {}",
                src.display(),
                dst.display(),
                e
            );
        });
    }
}
