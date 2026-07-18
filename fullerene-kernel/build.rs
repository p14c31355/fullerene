use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // ── Declare expected cfg flags ────────────────────────────────
    println!("cargo::rustc-check-cfg=cfg(have_ports_cpio)");

    // ── Propagate .driverignore cfg flags from Nitrogen ──────────
    let nitrogen_dir = manifest_dir.parent().unwrap().join("nitrogen");
    let ignore_path = nitrogen_dir.join(".driverignore");
    println!("cargo:rerun-if-changed={}", ignore_path.display());

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
                .map(|c| {
                    if c.is_alphanumeric() || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            if !clean.is_empty() {
                println!("cargo:rustc-cfg=nitrogen_no_{}", clean);
            }
        }
    }

    // ── Build port CPIO archive from toluene/<port>/app.bin ─────
    let toluene_dir = manifest_dir.parent().unwrap().join("toluene");
    let ports_cpio_path = out_dir.join("ports.cpio");
    let count = build_ports_cpio(&toluene_dir, &ports_cpio_path);
    if count > 0 {
        println!("cargo:rustc-cfg=have_ports_cpio");
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
    let wasm_src = manifest_dir
        .join("..")
        .join("toluene")
        .join("apps")
        .join("hello_wasi.rs");
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

/// Scan `toluene/` for subdirectories that contain `app.bin` and
/// produce a CPIO newc archive embedding them as port packages.
/// Returns the number of ports packed.
fn build_ports_cpio(toluene_dir: &Path, out: &Path) -> usize {
    let mut entries: Vec<(String, PortType, PathBuf)> = Vec::new();

    let dir_entries = match fs::read_dir(toluene_dir) {
        Ok(d) => d,
        Err(_) => return 0,
    };
    for entry in dir_entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_owned(),
            None => continue,
        };
        // Skip known non-port directories
        if name.starts_with('.')
            || name == "src"
            || name == "apps"
            || name == "rle_player"
            || name == "tests"
        {
            continue;
        }

        let bin_path = path.join("app.bin");
        if !bin_path.exists() {
            continue;
        }
        println!("cargo:rerun-if-changed={}", bin_path.display());

        let manifest_path = path.join("manifest.txt");
        // Determine runtime from manifest, default to "linux"
        let runtime = if manifest_path.exists() {
            let content = fs::read_to_string(&manifest_path).unwrap_or_default();
            if content.contains("runtime = \"native\"") {
                "native"
            } else {
                "linux"
            }
        } else {
            "linux"
        };
        let port_type = if runtime == "native" {
            PortType::Native
        } else {
            PortType::Linux
        };

        entries.push((name, port_type, bin_path));
    }

    if entries.is_empty() {
        return 0;
    }

    // Generate CPIO archive
    let mut buf = Vec::new();
    for (name, port_type, bin_path) in &entries {
        let data = fs::read(bin_path).unwrap_or_else(|e| {
            panic!("Failed to read {}: {}", bin_path.display(), e);
        });
        write_cpio_entry(&mut buf, name, *port_type, &data);
    }
    write_cpio_trailer(&mut buf);

    fs::write(out, &buf).unwrap_or_else(|e| {
        panic!("Failed to write CPIO archive to {}: {}", out.display(), e);
    });
    println!(
        "cargo:warning=Embedded {} port(s) via CPIO ({} bytes)",
        entries.len(),
        buf.len()
    );
    entries.len()
}

#[derive(Clone, Copy)]
enum PortType {
    Native,
    Linux,
}

/// Write one CPIO newc entry for a port package.
/// Creates both the directory entry and the two file entries
/// (manifest.txt + app.bin).
fn write_cpio_entry(buf: &mut Vec<u8>, name: &str, port_type: PortType, binary: &[u8]) {
    let runtime = match port_type {
        PortType::Native => "native",
        PortType::Linux => "linux",
    };
    let manifest = format!(
        "name = \"{name}\"\n\
         version = \"1.0.0\"\n\
         description = \"{name} port for Fullerene\"\n\
         binary = \"app.bin\"\n\
         runtime = \"{runtime}\"\n"
    );

    // Paths inside the archive
    let pkg_dir = format!("packages/{name}");
    let manifest_path = format!("packages/{name}/manifest.txt");
    let bin_path = format!("packages/{name}/app.bin");

    // 1. Directory: packages/<name>/
    write_cpio_file(buf, &pkg_dir, true, &[]);
    // 2. manifest.txt
    write_cpio_file(buf, &manifest_path, false, manifest.as_bytes());
    // 3. app.bin
    write_cpio_file(buf, &bin_path, false, binary);
}

/// Write a single CPIO newc entry (header + name + body).
fn write_cpio_file(buf: &mut Vec<u8>, archive_path: &str, is_dir: bool, body: &[u8]) {
    let name_bytes = archive_path.as_bytes();
    let name_len = name_bytes.len();
    let name_with_nul = name_len + 1;
    let name_padded = align4(name_with_nul);
    let body_padded = align4(body.len());

    // Header: 110 bytes
    let mode = if is_dir { 0o040755u32 } else { 0o100644u32 };
    let filesize = if is_dir { 0u64 } else { body.len() as u64 };

    write!(buf, "070701").unwrap();
    write_hex(buf, 1, 8); // ino
    write_hex(buf, mode as u64, 8); // mode
    write_hex(buf, 0, 8); // uid
    write_hex(buf, 0, 8); // gid
    write_hex(buf, if is_dir { 2 } else { 1 }, 8); // nlink
    write_hex(buf, 0, 8); // mtime
    write_hex(buf, filesize, 8); // filesize
    write_hex(buf, 0, 8); // devmajor
    write_hex(buf, 0, 8); // devminor
    write_hex(buf, 0, 8); // rdevmajor
    write_hex(buf, 0, 8); // rdevminor
    write_hex(buf, name_with_nul as u64, 8); // namesize
    write_hex(buf, 0, 8); // check

    // Name + NUL + padding
    buf.extend_from_slice(name_bytes);
    buf.push(0u8);
    for _ in name_with_nul..name_padded {
        buf.push(0u8);
    }

    // Body + padding
    buf.extend_from_slice(body);
    for _ in body.len()..body_padded {
        buf.push(0u8);
    }
}

/// Write the TRAILER!!! entry that terminates a CPIO archive.
fn write_cpio_trailer(buf: &mut Vec<u8>) {
    write!(buf, "070701").unwrap();
    // All numeric fields zero except namesize = 11
    for _ in 0..13 {
        write_hex(buf, 0, 8);
    }
    write_hex(buf, 11, 8); // namesize
    write_hex(buf, 0, 8); // check

    buf.extend_from_slice(b"TRAILER!!!\0");
    for _ in 0..(align4(11) - 11) {
        buf.push(0u8);
    }
}

fn write_hex(buf: &mut Vec<u8>, value: u64, digits: usize) {
    let s = format!("{:01$x}", value, digits);
    buf.extend_from_slice(s.as_bytes());
}

fn align4(n: usize) -> usize {
    (n + 3) & !3
}
