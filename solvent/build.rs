use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

    // ── Propagate .driverignore cfg flags from Nitrogen ──────────
    let nitrogen_dir = manifest_dir.parent().unwrap().join("nitrogen");
    let ignore_path = nitrogen_dir.join(".driverignore");
    println!("cargo:rerun-if-changed={}", ignore_path.display());

    // Read all driver names from .driverignore (both active and commented-out
    // entries) so we can register every possible cfg name up front.
    // Names are identified as single tokens consisting of [a-z_]+ optionally
    // followed by a trailing '/'.  Free-form comment text is ignored.
    // Separate tracking: `check_registered` for rustc-check-cfg dedup,
    // `cfg_emitted` for actual cfg emission dedup.
    let mut check_registered: Vec<String> = Vec::new();
    let mut cfg_emitted: Vec<String> = Vec::new();
    if ignore_path.exists() {
        let content = fs::read_to_string(&ignore_path).unwrap_or_default();
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let raw_name = trimmed.strip_prefix('#').unwrap_or(trimmed).trim();
            let mod_name = raw_name.strip_suffix('/').unwrap_or(raw_name);
            // Accept only simple [a-zA-Z_] identifiers to skip comment prose
            if mod_name.is_empty()
                || !mod_name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                continue;
            }
            let clean = mod_name.to_lowercase();
            if !check_registered.contains(&clean) {
                check_registered.push(clean.clone());
                println!("cargo::rustc-check-cfg=cfg(nitrogen_no_{})", clean);
            }
            if !trimmed.starts_with('#') && !cfg_emitted.contains(&clean) {
                println!("cargo:rustc-cfg=nitrogen_no_{}", clean);
                cfg_emitted.push(clean);
            }
        }
    }
}
