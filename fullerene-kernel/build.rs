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

    // ── Build application ports from submodule sources ──────────
    let toluene_dir = manifest_dir.parent().unwrap().join("toluene");
    let count = build_ports_cpio(&toluene_dir, &out_dir);
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

/// Build application ports from submodule sources and package into CPIO.
///
/// For each known port, this function:
/// 1. Locates its submodule under `toluene/<name>/`
/// 2. If the submodule has source, attempts to build it (or download a
///    pre‑built release) to produce a Linux ELF binary
/// 3. Caches the binary at `toluene/<name>/app.bin` so subsequent builds
///    skip the expensive source build
/// 4. Packages every successfully‑built port into a single CPIO archive
///
/// Returns the number of ports successfully packaged.
fn build_ports_cpio(toluene_dir: &Path, out_dir: &Path) -> usize {
    let mut prepared: Vec<(&str, PortType, Vec<u8>)> = Vec::new();

    for (name, builder) in KNOWN_PORTS.iter() {
        let submodule = toluene_dir.join(name);
        let cache = submodule.join("app.bin");

        // Try: cached binary already exists
        if cache.exists() {
            println!("cargo:rerun-if-changed={}", cache.display());
            if let Ok(data) = fs::read(&cache) {
                if is_valid_elf(&data) {
                    prepared.push((name, builder.runtime, data));
                    continue;
                }
            }
        }

        // Try: build from source
        println!("cargo:warning=ports: building {name}...");
        match (builder.build)(&submodule, out_dir) {
            Ok(data) => {
                let len = data.len();
                let _ = fs::write(&cache, &data);
                println!("cargo:rerun-if-changed={}", cache.display());
                prepared.push((name, builder.runtime, data));
                println!("cargo:warning=ports: {name} built ({} bytes)", len);
            }
            Err(msg) => {
                println!("cargo:warning=ports: {name} skipped – {msg}");
            }
        }
    }

    if prepared.is_empty() {
        return 0;
    }

    let mut buf = Vec::new();
    for (name, port_type, binary) in &prepared {
        write_cpio_package(&mut buf, name, *port_type, binary);
    }
    write_cpio_trailer(&mut buf);

    let out = out_dir.join("ports.cpio");
    fs::write(&out, &buf).unwrap_or_else(|e| {
        panic!("Failed to write CPIO archive to {}: {}", out.display(), e);
    });
    println!(
        "cargo:warning=Embedded {} port(s) via CPIO ({} bytes)",
        prepared.len(),
        buf.len()
    );
    prepared.len()
}

// ── Port registry ────────────────────────────────────────────────────

struct PortBuilder {
    runtime: PortType,
    /// Build the port from its submodule directory.
    /// Returns the binary bytes on success, or an error message on failure.
    build: fn(&Path, &Path) -> Result<Vec<u8>, &'static str>,
}

static KNOWN_PORTS: &[(&str, PortBuilder)] = &[
    (
        "cargo",
        PortBuilder {
            runtime: PortType::Linux,
            build: build_cargo,
        },
    ),
    (
        "freedoom",
        PortBuilder {
            runtime: PortType::Linux,
            build: build_freedoom,
        },
    ),
    (
        "netsurf",
        PortBuilder {
            runtime: PortType::Linux,
            build: build_netsurf,
        },
    ),
    (
        "vscodium",
        PortBuilder {
            runtime: PortType::Linux,
            build: build_vscodium,
        },
    ),
];

fn is_valid_elf(data: &[u8]) -> bool {
    data.len() >= 64 && data.starts_with(b"\x7fELF") && data.get(4) == Some(&2)
}

// ── Port‑specific build implementations ──────────────────────────────

/// Build cargo from submodule source via `cargo build --release`.
///
/// First build is slow (~1–2 min); subsequent builds are cached at
/// `toluene/cargo/app.bin` and reused instantly.
fn build_cargo(submodule: &Path, _out_dir: &Path) -> Result<Vec<u8>, &'static str> {
    if !submodule.join("Cargo.toml").exists() {
        return Err("submodule not cloned – run git submodule update --init");
    }
    let target_dir = submodule.join("target_ful");
    let status = Command::new("cargo")
        .args(["build", "--release", "--target-dir"])
        .arg(&target_dir)
        .current_dir(submodule)
        .status()
        .map_err(|_| "cargo command not found")?;
    if !status.success() {
        return Err("cargo build failed");
    }
    let bin = target_dir.join("release").join("cargo");
    let data = fs::read(&bin).map_err(|_| "cargo binary not produced")?;
    let _ = fs::remove_dir_all(&target_dir);
    Ok(data)
}

/// Build freedoom – produce WAD game data via `make`, then download a
/// statically‑linked Chocolate Doom engine, and bundle the WAD with it.
fn build_freedoom(submodule: &Path, out_dir: &Path) -> Result<Vec<u8>, &'static str> {
    if !submodule.join("Makefile").exists() {
        return Err("submodule not cloned");
    }

    // Build the WAD
    let status = Command::new("make")
        .current_dir(submodule)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|_| "make not found")?;
    if !status.success() {
        return Err("make failed – need deutex, python3, etc.");
    }
    let wad_path = submodule.join("wads").join("freedoom1.wad");
    if !wad_path.exists() {
        return Err("freedoom1.wad not produced");
    }
    let wad_data = fs::read(&wad_path).map_err(|_| "cannot read WAD")?;

    // Fetch (or use cached) Chocolate Doom engine
    let engine_cache = out_dir.join("chocolate-doom");
    let engine = if engine_cache.exists() {
        fs::read(&engine_cache).map_err(|_| "cannot read cached engine")?
    } else {
        fetch_chocolate_doom(&engine_cache)?
    };

    // Embed WAD after the engine
    let mut combined = engine;
    combined.extend_from_slice(b"FULLERENE_WAD");
    combined.extend_from_slice(&(wad_data.len() as u64).to_le_bytes());
    combined.extend_from_slice(&wad_data);
    Ok(combined)
}

/// Download (and cache) a statically‑linked Chocolate Doom x86_64 binary.
fn fetch_chocolate_doom(cache: &Path) -> Result<Vec<u8>, &'static str> {
    let url = "https://github.com/chocolate-doom/chocolate-doom/releases/download/3.1.0/chocolate-doom-3.1.0-x86_64-linux-gnu-static.tar.gz";
    let tmp = cache.with_extension("tar.gz");

    // Download
    let status = Command::new("curl")
        .args(["-sSL", "-o"])
        .arg(&tmp)
        .arg(url)
        .status()
        .map_err(|_| "curl not found")?;
    if !status.success() {
        return Err("curl download failed");
    }

    // Extract to a temp dir then find the binary
    let extract_dir = cache.parent().unwrap().join("choc_extract");
    let _ = fs::create_dir_all(&extract_dir);
    let tmp_str = tmp.to_string_lossy().into_owned();
    let ext_str = extract_dir.to_string_lossy().into_owned();
    let status = Command::new("tar")
        .args(["-xzf", &tmp_str, "-C", &ext_str])
        .status()
        .map_err(|_| "tar not found")?;
    if !status.success() {
        let _ = fs::remove_dir_all(&extract_dir);
        return Err("tar extraction failed");
    }

    // Find chocolate-doom binary
    let bin = find_in_dir(&extract_dir, "chocolate-doom")
        .ok_or("chocolate-doom binary not found in archive")?;
    let data = fs::read(&bin).map_err(|_| "cannot read engine binary")?;
    let _ = fs::copy(&bin, cache);
    let _ = fs::remove_dir_all(&extract_dir);
    let _ = fs::remove_file(&tmp);
    Ok(data)
}

fn find_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_in_dir(&p, name) {
                return Some(f);
            }
        } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(p);
        }
    }
    None
}

/// Build NetSurf – attempt `make`.
fn build_netsurf(submodule: &Path, _out_dir: &Path) -> Result<Vec<u8>, &'static str> {
    if !submodule.join("Makefile").exists() {
        return Err("submodule not cloned");
    }
    println!("cargo:warning=ports:   NetSurf requires gtk3, libcurl, openssl, libxml2-dev, etc.");
    let status = Command::new("make")
        .current_dir(submodule)
        .status()
        .map_err(|_| "make not found")?;
    if !status.success() {
        return Err("make failed (missing dependencies?)");
    }
    let candidates = ["netsurf", "nsbrowser", "build/release/netsurf"];
    for name in &candidates {
        let bin = submodule.join(name);
        if bin.exists() {
            return fs::read(&bin).map_err(|_| "cannot read binary");
        }
    }
    Err("netsurf binary not found after build")
}

/// Build VSCodium – this repo is a build‑config overlay over VS Code
/// proper; it doesn't contain the full Electron app source.  Building
/// requires cloning Microsoft/vscode into the expected subdirectory
/// and running the shell‑based pipeline.
fn build_vscodium(submodule: &Path, _out_dir: &Path) -> Result<Vec<u8>, &'static str> {
    if !submodule.join("build.sh").exists() {
        return Err("submodule not cloned");
    }
    println!("cargo:warning=ports:   VSCodium is a build overlay – see toluene/vscodium/build.sh");
    println!(
        "cargo:warning=ports:   Full build requires: git clone Microsoft/vscode + npm + electron"
    );
    println!("cargo:warning=ports:   Place the resulting binary at toluene/vscodium/app.bin");
    // Try to find a pre‑placed binary
    let bin = submodule.join("app.bin");
    if bin.exists() {
        return fs::read(&bin).map_err(|_| "cannot read app.bin");
    }
    Err("no pre‑built binary – build manually via toluene/vscodium/build.sh")
}

// ── Port data types ──────────────────────────────────────────────────

#[derive(Clone, Copy)]
#[allow(dead_code)]
enum PortType {
    Native,
    Linux,
}

// ── CPIO archive generation ─────────────────────────────────────────

/// Write a complete port package (directory + manifest + binary) to the CPIO archive.
fn write_cpio_package(buf: &mut Vec<u8>, name: &str, port_type: PortType, binary: &[u8]) {
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

    write_cpio_file(buf, &format!("packages/{name}"), true, &[]);
    write_cpio_file(
        buf,
        &format!("packages/{name}/manifest.txt"),
        false,
        manifest.as_bytes(),
    );
    write_cpio_file(buf, &format!("packages/{name}/app.bin"), false, binary);
}

/// Write a single CPIO newc entry (110‑byte header + padded name + padded body).
fn write_cpio_file(buf: &mut Vec<u8>, archive_path: &str, is_dir: bool, body: &[u8]) {
    let name_bytes = archive_path.as_bytes();
    let name_with_nul = name_bytes.len() + 1;
    let name_padded = align4(name_with_nul);
    let body_padded = align4(body.len());

    let mode = if is_dir { 0o040755u32 } else { 0o100644u32 };
    let filesize = if is_dir { 0u64 } else { body.len() as u64 };

    write!(buf, "070701").unwrap();
    write_hex(buf, 1, 8);
    write_hex(buf, mode as u64, 8);
    write_hex(buf, 0, 8);
    write_hex(buf, 0, 8);
    write_hex(buf, if is_dir { 2 } else { 1 }, 8);
    write_hex(buf, 0, 8);
    write_hex(buf, filesize, 8);
    write_hex(buf, 0, 8);
    write_hex(buf, 0, 8);
    write_hex(buf, 0, 8);
    write_hex(buf, 0, 8);
    write_hex(buf, name_with_nul as u64, 8);
    write_hex(buf, 0, 8);

    buf.extend_from_slice(name_bytes);
    buf.push(0u8);
    for _ in name_with_nul..name_padded {
        buf.push(0u8);
    }

    buf.extend_from_slice(body);
    for _ in body.len()..body_padded {
        buf.push(0u8);
    }
}

/// Write the TRAILER!!! entry.
fn write_cpio_trailer(buf: &mut Vec<u8>) {
    write!(buf, "070701").unwrap();
    for _ in 0..13 {
        write_hex(buf, 0, 8);
    }
    write_hex(buf, 11, 8);
    write_hex(buf, 0, 8);
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
