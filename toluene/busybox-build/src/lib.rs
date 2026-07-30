//! Shared host-side BusyBox build and validation workflow.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

const SOURCE_MARKER_SUFFIX: &str = ".source-revision";
const LOCK_SUFFIX: &str = ".lock";

#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub source: PathBuf,
    pub build_dir: PathBuf,
    pub output: PathBuf,
    pub compiler: Option<OsString>,
    pub jobs: Option<usize>,
    pub clean: bool,
}

/// Build, validate, and stage a static x86_64 BusyBox binary.
///
/// A validated output is reused when its marker matches the checked-out
/// BusyBox revision. The lock protects the shared staged output while each
/// caller gets its own out-of-tree build directory.
pub fn build(options: &BuildOptions) -> Result<(), String> {
    if let Some(parent) = options.output.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "cannot create BusyBox output directory {}: {error}",
                parent.display()
            )
        })?;
    }
    let marker = source_marker_path(&options.output);
    let lock_path = options
        .output
        .with_extension(LOCK_SUFFIX.trim_start_matches('.'));
    let _lock = BuildLock::acquire(&lock_path)?;

    if output_is_current(&options.output, &marker, &options.source) {
        if options.clean && options.build_dir.exists() {
            fs::remove_dir_all(&options.build_dir).map_err(|error| {
                format!(
                    "cannot clean BusyBox build directory {}: {error}",
                    options.build_dir.display()
                )
            })?;
        }
        return Ok(());
    }

    if !options.source.join("Makefile").is_file() {
        return Err(format!(
            "BusyBox source is missing at {}. Initialize submodules with `git submodule update --init --recursive`",
            options.source.display()
        ));
    }
    let compiler = options
        .compiler
        .clone()
        .or_else(|| env::var_os("FULLERENE_BUSYBOX_CC"))
        .or_else(|| command_available("musl-gcc").then(|| OsString::from("musl-gcc")))
        .unwrap_or_else(|| OsString::from("gcc"));

    if options.clean && options.build_dir.exists() {
        fs::remove_dir_all(&options.build_dir).map_err(|error| {
            format!(
                "cannot clean BusyBox build directory {}: {error}",
                options.build_dir.display()
            )
        })?;
    }
    fs::create_dir_all(&options.build_dir).map_err(|error| {
        format!(
            "cannot create BusyBox build directory {}: {error}",
            options.build_dir.display()
        )
    })?;

    let config_path = options.build_dir.join(".config");
    run_make(
        &options.source,
        &options.build_dir,
        &compiler,
        options.jobs,
        &["defconfig"],
    )?;
    configure_for_fullerene(&config_path)?;

    eprintln!(
        "Building static BusyBox with {} into {}",
        compiler.to_string_lossy(),
        options.build_dir.display()
    );
    run_make(
        &options.source,
        &options.build_dir,
        &compiler,
        options.jobs,
        &[],
    )?;

    let built = options.build_dir.join("busybox");
    let data = fs::read(&built)
        .map_err(|error| format!("cannot read built BusyBox {}: {error}", built.display()))?;
    if !is_static_x86_64_elf(&data) {
        return Err(format!(
            "BusyBox output is not a static x86_64 ELF: {}",
            built.display()
        ));
    }

    if let Some(parent) = options.output.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "cannot create BusyBox output directory {}: {error}",
                parent.display()
            )
        })?;
    }
    fs::copy(&built, &options.output).map_err(|error| {
        format!(
            "cannot copy BusyBox to {}: {error}",
            options.output.display()
        )
    })?;
    make_executable(&options.output)?;
    fs::write(
        &marker,
        source_revision(&options.source).unwrap_or_default(),
    )
    .map_err(|error| {
        format!(
            "cannot write BusyBox source marker {}: {error}",
            marker.display()
        )
    })?;

    if options.clean {
        fs::remove_dir_all(&options.build_dir).map_err(|error| {
            format!(
                "cannot clean BusyBox build directory {} after build: {error}",
                options.build_dir.display()
            )
        })?;
    }
    Ok(())
}

fn source_marker_path(output: &Path) -> PathBuf {
    PathBuf::from(format!("{}{}", output.display(), SOURCE_MARKER_SUFFIX))
}

fn output_is_current(output: &Path, marker: &Path, source: &Path) -> bool {
    let Ok(data) = fs::read(output) else {
        return false;
    };
    let Some(revision) = source_revision(source) else {
        return false;
    };
    is_static_x86_64_elf(&data)
        && fs::read_to_string(marker)
            .map(|stored| stored.trim() == revision)
            .unwrap_or(false)
}

fn source_revision(source: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", source.to_str()?, "rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let revision = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!revision.is_empty()).then_some(revision)
}

fn command_available(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn run_make(
    source: &Path,
    build_dir: &Path,
    compiler: &OsStr,
    jobs: Option<usize>,
    targets: &[&str],
) -> Result<(), String> {
    let mut command = Command::new("make");
    command
        .current_dir(source)
        .arg(format!("O={}", build_dir.display()))
        .arg(format!("CC={}", compiler.to_string_lossy()))
        .env("LC_ALL", "C")
        .stdin(Stdio::null());
    if let Some(jobs) = jobs {
        command.arg(format!("-j{jobs}"));
    }
    let status = command
        .args(targets)
        .status()
        .map_err(|error| format!("failed to start make: {error}"))?;
    if !status.success() {
        return Err(format!("BusyBox make {:?} failed with {status}", targets));
    }
    Ok(())
}

fn configure_for_fullerene(path: &Path) -> Result<(), String> {
    let original = fs::read_to_string(path)
        .map_err(|error| format!("cannot read BusyBox config {}: {error}", path.display()))?;
    let mut config = original.clone();
    for (key, value) in [
        ("CONFIG_STATIC", "y"),
        ("CONFIG_STATIC_LIBGCC", "y"),
        ("CONFIG_PIE", "n"),
        ("CONFIG_FEATURE_PREFER_APPLETS", "y"),
        ("CONFIG_FEATURE_SH_STANDALONE", "y"),
        ("CONFIG_FEATURE_SH_EXTRA_QUIET", "y"),
        ("CONFIG_BUSYBOX_EXEC_PATH", "\"/bin/busybox\""),
        ("CONFIG_TC", "n"),
    ] {
        set_config_value(&mut config, key, value);
    }
    if config != original {
        fs::write(path, config)
            .map_err(|error| format!("cannot write BusyBox config {}: {error}", path.display()))?;
    }
    Ok(())
}

fn set_config_value(config: &mut String, key: &str, value: &str) {
    let replacement = if value == "n" {
        format!("# {key} is not set")
    } else {
        format!("{key}={value}")
    };
    let mut found = false;
    let mut rewritten = String::with_capacity(config.len() + replacement.len());
    for line in config.lines() {
        if line.starts_with(&format!("{key}=")) || line == format!("# {key} is not set") {
            rewritten.push_str(&replacement);
            found = true;
        } else {
            rewritten.push_str(line);
        }
        rewritten.push('\n');
    }
    if !found {
        rewritten.push_str(&replacement);
        rewritten.push('\n');
    }
    *config = rewritten;
}

pub fn is_static_x86_64_elf(data: &[u8]) -> bool {
    if data.len() < 64
        || !data.starts_with(b"\x7fELF")
        || data.get(4) != Some(&2)
        || data.get(5) != Some(&1)
        || data.get(18..20) != Some(&[0x3e, 0])
    {
        return false;
    }
    let e_type = u16::from_le_bytes([data[16], data[17]]);
    if e_type != 2 && e_type != 3 {
        return false;
    }
    let Some(phoff) = usize::try_from(u64::from_le_bytes(data[32..40].try_into().unwrap())).ok()
    else {
        return false;
    };
    let phentsize = usize::from(u16::from_le_bytes([data[54], data[55]]));
    let phnum = usize::from(u16::from_le_bytes([data[56], data[57]]));
    if phentsize < 4 {
        return false;
    }
    for index in 0..phnum {
        let Some(offset) = phoff.checked_add(index.saturating_mul(phentsize)) else {
            return false;
        };
        let Some(entry_end) = offset.checked_add(4) else {
            return false;
        };
        let Some(entry) = data.get(offset..entry_end) else {
            return false;
        };
        if entry == [3, 0, 0, 0] {
            return false;
        }
    }
    true
}

fn make_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .map_err(|error| format!("cannot stat {}: {error}", path.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)
            .map_err(|error| format!("cannot make {} executable: {error}", path.display()))?;
    }
    Ok(())
}

struct BuildLock {
    path: PathBuf,
}

impl BuildLock {
    fn acquire(path: &Path) -> Result<Self, String> {
        for _ in 0..6000 {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(_) => {
                    return Ok(Self {
                        path: path.to_owned(),
                    });
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(error) => {
                    return Err(format!(
                        "cannot create BusyBox build lock {}: {error}",
                        path.display()
                    ));
                }
            }
        }
        Err(format!(
            "timed out waiting for BusyBox build lock {}",
            path.display()
        ))
    }
}

impl Drop for BuildLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::{is_static_x86_64_elf, set_config_value};

    #[test]
    fn config_values_replace_disabled_symbols() {
        let mut config = "# CONFIG_STATIC is not set\nCONFIG_TC=y\n".to_string();
        set_config_value(&mut config, "CONFIG_STATIC", "y");
        set_config_value(&mut config, "CONFIG_TC", "n");
        assert_eq!(config, "CONFIG_STATIC=y\n# CONFIG_TC is not set\n");
    }

    #[test]
    fn rejects_non_elf_data() {
        assert!(!is_static_x86_64_elf(b"not an ELF"));
    }
}
