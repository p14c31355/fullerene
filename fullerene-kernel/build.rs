use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // ── Propagate .driverignore cfg flags from Nitrogen ──────────
    let nitrogen_dir = manifest_dir.parent().unwrap().join("nitrogen");
    let ignore_path = nitrogen_dir.join(".driverignore");
    println!("cargo:rerun-if-changed={}", ignore_path.display());

    let known_drivers = &[
        "audio", "framebuffer", "hda", "ioapic", "iommu",
        "iwlwifi", "pic", "ps2", "storage", "usb", "virtio", "wifi",
    ];
    for name in known_drivers {
        println!("cargo::rustc-check-cfg=cfg(nitrogen_no_{})", name);
    }

    if ignore_path.exists() {
        let content = fs::read_to_string(&ignore_path).unwrap_or_default();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mod_name = line.strip_suffix('/').unwrap_or(line);
            let clean: String = mod_name
                .chars()
                .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
                .collect();
            if !clean.is_empty() {
                println!("cargo:rustc-cfg=nitrogen_no_{}", clean);
            }
        }
    }

    // ── Copy existing media assets ───────────────────────────────
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

    // ── Build WASI test app ──────────────────────────────────────
    let wasm_src = manifest_dir.join("..").join("toluene").join("apps").join("hello_wasi.rs");
    let wasm_out = out_dir.join("hello.wasm");

    println!("cargo:rerun-if-changed={}", wasm_src.display());

    // Use the RUSTC from cargo's build environment — it points to the correct
    // toolchain (respecting rust-toolchain.toml). Derive sysroot from it.
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());

    let sysroot = String::from_utf8(
        Command::new(&rustc)
            .args(["--print", "sysroot"])
            .output()
            .expect("Failed to get sysroot from rustc")
            .stdout,
    )
    .expect("Invalid UTF-8 from rustc --print sysroot")
    .trim()
    .to_string();

    let status = Command::new(&rustc)
        .args([
            "--target",
            "wasm32-wasip1",
            "--sysroot",
            &sysroot,
            "-C",
            "opt-level=s",
            "-C",
            "lto=yes",
            "-o",
        ])
        .arg(&wasm_out)
        .arg(&wasm_src)
        .status()
        .expect("Failed to execute rustc for WASM build");

    if !status.success() {
        panic!(
            "Failed to compile WASI test app from '{}'. \
             Make sure the wasm32-wasip1 target is installed: \
             rustup target add wasm32-wasip1",
            wasm_src.display()
        );
    }
}