//! Shared host-side BusyBox build and validation workflow.

use std::collections::BTreeSet;
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
// Bump when configure_for_fullerene changes. The staged output must not reuse
// a binary built with an older feature configuration merely because the
// BusyBox source revision stayed the same.
const BUILD_CONFIG_REVISION: &str = "fullerene-busybox-config-v5-dynamic-glibc-ash-help";

/// Applets covered by the Fullerene Linux personality contract.
///
/// Keep this list deliberately small and explicit.  BusyBox's `defconfig`
/// enables hundreds of hardware, networking, and service applets which cannot
/// be meaningful on Fullerene until their corresponding Linux ABI exists.
/// The generated binary must never advertise an unverified applet through
/// `busybox --help`.
pub const FULLERENE_BUSYBOX_APPLETS: &[(&str, &str)] = &[
    ("[", "TEST1"),
    ("[[", "TEST2"),
    ("ash", "ASH"),
    ("arch", "BB_ARCH"),
    ("awk", "AWK"),
    ("basename", "BASENAME"),
    ("busybox", "BUSYBOX"),
    ("cat", "CAT"),
    ("cksum", "CKSUM"),
    ("clear", "CLEAR"),
    ("cp", "CP"),
    ("cut", "CUT"),
    ("date", "DATE"),
    ("dd", "DD"),
    ("dirname", "DIRNAME"),
    ("echo", "ECHO"),
    ("env", "ENV"),
    ("expr", "EXPR"),
    ("false", "FALSE"),
    ("fold", "FOLD"),
    ("grep", "GREP"),
    ("head", "HEAD"),
    ("hexdump", "HEXDUMP"),
    ("hostname", "HOSTNAME"),
    ("ls", "LS"),
    ("md5sum", "MD5SUM"),
    ("mkdir", "MKDIR"),
    ("mktemp", "MKTEMP"),
    ("mv", "MV"),
    ("od", "OD"),
    ("printenv", "PRINTENV"),
    ("printf", "PRINTF"),
    ("pwd", "PWD"),
    ("rm", "RM"),
    ("rmdir", "RMDIR"),
    ("sed", "SED"),
    ("seq", "SEQ"),
    ("sha256sum", "SHA256SUM"),
    ("sh", "SH_IS_ASH"),
    ("sleep", "SLEEP"),
    ("sort", "SORT"),
    ("stat", "STAT"),
    ("tail", "TAIL"),
    ("tar", "TAR"),
    ("tee", "TEE"),
    ("test", "TEST"),
    ("touch", "TOUCH"),
    ("tr", "TR"),
    ("true", "TRUE"),
    ("tty", "TTY"),
    ("uname", "UNAME"),
    ("uniq", "UNIQ"),
    ("uptime", "UPTIME"),
    ("wc", "WC"),
    ("which", "WHICH"),
    ("whoami", "WHOAMI"),
    ("yes", "YES"),
];

/// Return the applet names in the same order used by the build contract.
pub fn fullerene_busybox_applet_names() -> impl Iterator<Item = &'static str> {
    FULLERENE_BUSYBOX_APPLETS.iter().map(|(name, _)| *name)
}

#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub source: PathBuf,
    pub build_dir: PathBuf,
    pub output: PathBuf,
    pub compiler: Option<OsString>,
    pub jobs: Option<usize>,
    pub clean: bool,
}

/// Build, validate, and stage a dynamically linked glibc x86_64 BusyBox binary.
///
/// A validated output is reused when its marker matches the checked-out
/// BusyBox revision and Fullerene build configuration. The lock protects the
/// shared staged output while each caller gets its own out-of-tree build
/// directory.
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
        &["allnoconfig"],
    )?;
    configure_for_fullerene(&config_path, &options.source)?;
    run_make(
        &options.source,
        &options.build_dir,
        &compiler,
        options.jobs,
        &["oldconfig"],
    )?;

    eprintln!(
        "Building dynamically linked glibc BusyBox with {} into {}",
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
    if !is_dynamic_glibc_x86_64_elf(&data) {
        return Err(format!(
            "BusyBox output is not a dynamically linked glibc x86_64 ELF: {}",
            built.display()
        ));
    }
    validate_fullerene_busybox(&built)?;

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
    fs::write(&marker, build_marker(&options.source).unwrap_or_default()).map_err(|error| {
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

fn build_marker(source: &Path) -> Option<String> {
    source_revision(source).map(|revision| format!("{revision}\n{BUILD_CONFIG_REVISION}"))
}

fn output_is_current(output: &Path, marker: &Path, source: &Path) -> bool {
    let Ok(data) = fs::read(output) else {
        return false;
    };
    let Some(expected_marker) = build_marker(source) else {
        return false;
    };
    is_dynamic_glibc_x86_64_elf(&data)
        && fs::read_to_string(marker)
            .map(|stored| stored.trim() == expected_marker.trim())
            .unwrap_or(false)
        && validate_fullerene_busybox(output).is_ok()
}

/// Ensure a candidate binary advertises exactly the applets in the Fullerene
/// contract.  This check is deliberately performed on reused and externally
/// supplied binaries too; otherwise a stale or host-provided BusyBox could
/// silently reintroduce unsupported commands into `busybox --help`.
pub fn validate_fullerene_busybox(path: &Path) -> Result<(), String> {
    let data = fs::read(path)
        .map_err(|error| format!("cannot read BusyBox {}: {error}", path.display()))?;
    let actual: BTreeSet<String> = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        match Command::new(path).arg("--list").output() {
            Ok(output) if output.status.success() => std::str::from_utf8(&output.stdout)
                .map_err(|error| format!("BusyBox --list output is not UTF-8: {error}"))?
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
            _ => applet_table_from_elf(&data)?,
        }
    } else {
        applet_table_from_elf(&data)?
    };
    let expected: BTreeSet<String> = fullerene_busybox_applet_names()
        .map(str::to_owned)
        .collect();
    if actual != expected {
        let missing: Vec<_> = expected.difference(&actual).cloned().collect();
        let extra: Vec<_> = actual.difference(&expected).cloned().collect();
        return Err(format!(
            "BusyBox applet contract mismatch for {} (missing: {}; extra: {})",
            path.display(),
            missing.join(","),
            extra.join(",")
        ));
    }
    Ok(())
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
    let mut feeder = None;
    if targets == ["oldconfig"] {
        let mut yes = Command::new("yes")
            .arg("")
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| format!("failed to start config-default feeder: {error}"))?;
        let stdout = yes
            .stdout
            .take()
            .ok_or_else(|| "config-default feeder has no stdout".to_string())?;
        command.stdin(Stdio::from(stdout));
        feeder = Some(yes);
    }
    let status = command
        .args(targets)
        .status()
        .map_err(|error| format!("failed to start make: {error}"))?;
    if let Some(mut feeder) = feeder {
        let _ = feeder.kill();
        let _ = feeder.wait();
    }
    if !status.success() {
        return Err(format!("BusyBox make {:?} failed with {status}", targets));
    }
    Ok(())
}

fn configure_for_fullerene(path: &Path, source: &Path) -> Result<(), String> {
    let original = fs::read_to_string(path)
        .map_err(|error| format!("cannot read BusyBox config {}: {error}", path.display()))?;
    let mut config = original.clone();
    for (key, value) in [
        ("CONFIG_BUSYBOX", "y"),
        ("CONFIG_STATIC", "n"),
        ("CONFIG_STATIC_LIBGCC", "n"),
        ("CONFIG_PIE", "n"),
        ("CONFIG_FEATURE_PREFER_APPLETS", "y"),
        ("CONFIG_FEATURE_SH_STANDALONE", "y"),
        ("CONFIG_FEATURE_SH_NOFORK", "y"),
        ("CONFIG_FEATURE_SH_EXTRA_QUIET", "y"),
        ("CONFIG_FEATURE_EDITING", "y"),
        ("CONFIG_FEATURE_TAB_COMPLETION", "y"),
        ("CONFIG_BUSYBOX_EXEC_PATH", "\"/bin/busybox\""),
        // The tar applet is part of the contract, so its normal archive
        // creation path must be present instead of only the dispatcher/help
        // entry.
        ("CONFIG_FEATURE_TAR_LONG_OPTIONS", "y"),
        ("CONFIG_FEATURE_TAR_CREATE", "y"),
    ] {
        set_config_value(&mut config, key, value);
    }
    for (_, symbol) in FULLERENE_BUSYBOX_APPLETS {
        if busybox_defines_symbol(source, symbol) {
            set_config_value(&mut config, &format!("CONFIG_{symbol}"), "y");
        }
    }
    // `sh` is the ash shell selected by the applet contract.  These options
    // are intentionally explicit because allnoconfig otherwise leaves the
    // shell's internal support disabled.
    for (key, value) in [
        ("CONFIG_ASH", "y"),
        ("CONFIG_SHELL_ASH", "y"),
        ("CONFIG_ASH_OPTIMIZE_FOR_SIZE", "y"),
        ("CONFIG_ASH_INTERNAL_GLOB", "y"),
        ("CONFIG_ASH_ECHO", "y"),
        ("CONFIG_ASH_PRINTF", "y"),
        ("CONFIG_ASH_TEST", "y"),
        ("CONFIG_ASH_HELP", "y"),
    ] {
        set_config_value(&mut config, key, value);
    }
    if config != original {
        fs::write(path, config)
            .map_err(|error| format!("cannot write BusyBox config {}: {error}", path.display()))?;
    }
    Ok(())
}

fn busybox_defines_symbol(source: &Path, symbol: &str) -> bool {
    let config_name = format!("CONFIG_{symbol}");
    let config_line = format!("config {symbol}");
    let mut pending = vec![source.to_owned()];
    while let Some(path) = pending.pop() {
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            let Ok(entries) = fs::read_dir(&path) else {
                continue;
            };
            for entry in entries.flatten() {
                if entry.file_name() != ".git" {
                    pending.push(entry.path());
                }
            }
        } else if let Ok(contents) = fs::read_to_string(&path)
            && contents.lines().any(|line| {
                let trimmed = line.trim_start_matches('/').trim_start();
                trimmed.starts_with(&config_line)
                    || trimmed.contains(&format!("//config:{config_line}"))
                    || trimmed.contains(&config_name)
            })
        {
            return true;
        }
    }
    false
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

pub fn is_dynamic_glibc_x86_64_elf(data: &[u8]) -> bool {
    dynamic_glibc_interpreter_path(data).is_some()
}

/// Return the accepted glibc interpreter path recorded by PT_INTERP.
pub fn dynamic_glibc_interpreter_path(data: &[u8]) -> Option<&'static str> {
    if data.len() < 64
        || !data.starts_with(b"\x7fELF")
        || data.get(4) != Some(&2)
        || data.get(5) != Some(&1)
        || u16::from_le_bytes([data[18], data[19]]) != 0x3e
        || !matches!(u16::from_le_bytes([data[16], data[17]]), 2 | 3)
    {
        return None;
    }
    let phoff = usize::try_from(u64::from_le_bytes(data[32..40].try_into().ok()?)).ok()?;
    let phentsize = usize::from(u16::from_le_bytes([data[54], data[55]]));
    let phnum = usize::from(u16::from_le_bytes([data[56], data[57]]));
    if phentsize < 56 {
        return None;
    }
    for index in 0..phnum {
        let offset = phoff.checked_add(index.checked_mul(phentsize)?)?;
        let entry = data.get(offset..offset.checked_add(56)?)?;
        if u32::from_le_bytes(entry[0..4].try_into().ok()?) != 3 {
            continue;
        }
        let p_offset = usize::try_from(u64::from_le_bytes(entry[8..16].try_into().ok()?)).ok()?;
        let p_filesz = usize::try_from(u64::from_le_bytes(entry[32..40].try_into().ok()?)).ok()?;
        let bytes = data.get(p_offset..p_offset.checked_add(p_filesz)?)?;
        let nul = bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len());
        return match bytes.get(..nul)? {
            b"/lib64/ld-linux-x86-64.so.2" => Some("/lib64/ld-linux-x86-64.so.2"),
            b"/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2" => {
                Some("/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2")
            }
            _ => None,
        };
    }
    None
}

fn applet_table_from_elf(data: &[u8]) -> Result<BTreeSet<String>, String> {
    // BusyBox emits its sorted applet-name table as a NUL-separated ELF
    // string run. The leading `[`/`[[` pair is specific enough to avoid
    // treating diagnostic prose or shell keywords as the applet table.
    let marker = b"[\0[[\0arch\0";
    let start = data
        .windows(marker.len())
        .position(|window| window == marker)
        .ok_or_else(|| "BusyBox applet table was not found in ELF data".to_string())?;
    let mut actual = BTreeSet::new();
    for name in data[start..]
        .split(|byte| *byte == 0)
        .take_while(|name| !name.is_empty())
    {
        let name = std::str::from_utf8(name)
            .map_err(|error| format!("BusyBox applet table is not UTF-8: {error}"))?;
        actual.insert(name.to_owned());
    }
    if actual.is_empty() {
        return Err("BusyBox applet table is empty".to_string());
    }
    Ok(actual)
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
    use super::{dynamic_glibc_interpreter_path, is_dynamic_glibc_x86_64_elf, set_config_value};

    fn synthetic_elf(interpreter: &[u8]) -> Vec<u8> {
        let phoff = 64usize;
        let interp_offset = phoff + 56;
        let mut data = vec![0u8; interp_offset + interpreter.len() + 1];
        data[..4].copy_from_slice(b"\x7fELF");
        data[4] = 2;
        data[5] = 1;
        data[16..18].copy_from_slice(&2u16.to_le_bytes());
        data[18..20].copy_from_slice(&0x3eu16.to_le_bytes());
        data[32..40].copy_from_slice(&(phoff as u64).to_le_bytes());
        data[54..56].copy_from_slice(&56u16.to_le_bytes());
        data[56..58].copy_from_slice(&1u16.to_le_bytes());
        data[phoff..phoff + 4].copy_from_slice(&3u32.to_le_bytes());
        data[phoff + 8..phoff + 16].copy_from_slice(&(interp_offset as u64).to_le_bytes());
        data[phoff + 32..phoff + 40]
            .copy_from_slice(&((interpreter.len() + 1) as u64).to_le_bytes());
        data[interp_offset..interp_offset + interpreter.len()].copy_from_slice(interpreter);
        data[interp_offset + interpreter.len()] = 0;
        data
    }

    #[test]
    fn config_values_replace_disabled_symbols() {
        let mut config = "# CONFIG_STATIC is not set\nCONFIG_TC=y\n".to_string();
        set_config_value(&mut config, "CONFIG_STATIC", "y");
        set_config_value(&mut config, "CONFIG_TC", "n");
        assert_eq!(config, "CONFIG_STATIC=y\n# CONFIG_TC is not set\n");
    }

    #[test]
    fn rejects_non_elf_data() {
        assert!(!is_dynamic_glibc_x86_64_elf(b"not an ELF"));
    }

    #[test]
    fn accepts_supported_dynamic_glibc_interpreters() {
        for path in [
            b"/lib64/ld-linux-x86-64.so.2".as_slice(),
            b"/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2".as_slice(),
        ] {
            assert_eq!(
                dynamic_glibc_interpreter_path(&synthetic_elf(path)),
                Some(std::str::from_utf8(path).unwrap())
            );
        }
    }

    #[test]
    fn rejects_unsupported_dynamic_interpreter() {
        assert_eq!(
            dynamic_glibc_interpreter_path(&synthetic_elf(b"/lib/ld-musl-x86_64.so.1")),
            None
        );
    }
}
