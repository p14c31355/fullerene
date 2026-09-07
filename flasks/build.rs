#[path = "src/udev_spec.rs"]
mod udev_spec;

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=src/udev_spec.rs");

    // OUT_DIR is Cargo's target/.../build/.../out directory.  The rules are
    // generated there and then embedded into the flasks binary at compile
    // time; no generated deployment artifact is kept in the repository.
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo did not set OUT_DIR"));
    let rules_path = out_dir.join(udev_spec::RULE_FILE_NAME);
    fs::write(&rules_path, udev_spec::render()).expect("failed to generate udev rules");
    println!(
        "cargo:rustc-env=FULLERENE_UDEV_RULES={}",
        rules_path.display()
    );
}
