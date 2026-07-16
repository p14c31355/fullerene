//! Build script for Nitrogen — `.driverignore`‑based module selection.
//!
//! Reads `.driverignore` from the crate root, and emits `cargo:rustc-cfg=`
//! flags for each ignored driver module.  The flags are used in `lib.rs`
//! to conditionally `pub mod` or skip each driver.
//!
//! # How it works
//!
//! 1. Read `.driverignore` (one ignore‑pattern per line; `#` comments
//!    and blank lines are skipped; trailing `/` is stripped).
//! 2. For each ignored driver module name, emit a cfg like
//!    `cargo:rustc-cfg=nitrogen_no_usb`.
//! 3. `lib.rs` uses `#[cfg(not(nitrogen_no_usb))] pub mod usb;` etc.
//!
//! Infrastructure modules (`pci`, `driver_api`, `driver_context`, …)
//! are never emitted as skip‑flags — they are always compiled.

use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let ignore_path = Path::new(&manifest_dir).join(".driverignore");

    // ── Read .driverignore ───────────────────────────────────
    let ignored: Vec<String> = if ignore_path.exists() {
        let content = fs::read_to_string(&ignore_path).unwrap_or_default();
        content
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| line.strip_suffix('/').unwrap_or(line).to_string())
            .collect()
    } else {
        Vec::new()
    };

    // ── Emit cfg flags — one per ignored driver ──────────────
    // Each skipped module gets a `nitrogen_no_<module>` cfg flag.
    //
    // Also declare all possible cfg names so nightly rustc doesn't warn.

    // Shared list of known driver modules (must match lib.rs gated modules).
    let known_drivers = &[
        "audio",
        "framebuffer",
        "hda",
        "ioapic",
        "iommu",
        "iwlwifi",
        "pic",
        "ps2",
        "storage",
        "usb",
        "virtio",
        "wifi",
    ];

    // Declare all possible cfg names up front.
    for name in known_drivers {
        println!("cargo::rustc-check-cfg=cfg(nitrogen_no_{})", name);
    }

    for mod_name in &ignored {
        // Sanitize: module names use underscores, cfg flags follow the same.
        let clean: String = mod_name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();

        // Validate against known driver list.
        if !known_drivers.contains(&clean.as_str()) {
            println!(
                "cargo:warning=.driverignore: unknown module '{}' (will be ignored)",
                mod_name
            );
            continue;
        }

        println!("cargo:rustc-cfg=nitrogen_no_{}", clean);
    }

    // ── Rebuild when driver selection changes ────────────────
    println!("cargo:rerun-if-changed={}", ignore_path.display());
    println!("cargo:rerun-if-changed=build.rs");
}
