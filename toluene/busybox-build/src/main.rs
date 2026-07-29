//! Build the POSIX BusyBox binary used by Fullerene's Linux personality.
//!
//! The BusyBox source is intentionally kept as a Git submodule. This tool
//! owns the host-side build setup, while the kernel build only consumes the
//! resulting, validated ELF file.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DEFAULT_OUTPUT: &str = "target/busybox/busybox";
const DEFAULT_BUILD_DIR: &str = "target/busybox-build";

#[derive(Debug)]
struct Options {
    source: Option<PathBuf>,
    build_dir: Option<PathBuf>,
    output: Option<PathBuf>,
    compiler: Option<OsString>,
    jobs: Option<usize>,
    clean: bool,
}

fn main() -> Result<(), String> {
    let workspace = workspace_root();
    let options = parse_options()?;

    let source = absolute_path(
        options
            .source
            .unwrap_or_else(|| workspace.join("toluene/busybox")),
        &workspace,
    );
    let build_dir = absolute_path(
        options
            .build_dir
            .unwrap_or_else(|| workspace.join(DEFAULT_BUILD_DIR)),
        &workspace,
    );
    let output = absolute_path(
        options
            .output
            .unwrap_or_else(|| workspace.join(DEFAULT_OUTPUT)),
        &workspace,
    );
    if options.clean && output.starts_with(&build_dir) {
        return Err(format!(
            "--output {} must not be inside --build-dir {} when --clean is used",
            output.display(),
            build_dir.display()
        ));
    }
    let jobs = options.jobs.or_else(|| {
        std::thread::available_parallelism()
            .ok()
            .map(|count| std::num::NonZeroUsize::get(count).min(8))
    });

    if !source.join("Makefile").is_file() {
        return Err(format!(
            "BusyBox source is missing at {}. Initialize submodules with `git submodule update --init --recursive`.",
            source.display()
        ));
    }

    let compiler = options
        .compiler
        .or_else(|| env::var_os("FULLERENE_BUSYBOX_CC"))
        .or_else(|| command_available("musl-gcc").then(|| OsString::from("musl-gcc")))
        .unwrap_or_else(|| OsString::from("gcc"));

    if options.clean && build_dir.exists() {
        fs::remove_dir_all(&build_dir).map_err(|error| {
            format!(
                "cannot clean BusyBox build directory {}: {error}",
                build_dir.display()
            )
        })?;
    }
    fs::create_dir_all(&build_dir).map_err(|error| {
        format!(
            "cannot create BusyBox build directory {}: {error}",
            build_dir.display()
        )
    })?;

    let config_path = build_dir.join(".config");
    if !config_path.exists() {
        run_make(&source, &build_dir, &compiler, jobs, &["defconfig"])?;
    }
    configure_for_fullerene(&config_path)?;

    eprintln!(
        "Building static BusyBox with {} into {}",
        compiler.to_string_lossy(),
        build_dir.display()
    );
    run_make(&source, &build_dir, &compiler, jobs, &[])?;

    let built = build_dir.join("busybox");
    let data = fs::read(&built)
        .map_err(|error| format!("cannot read built BusyBox {}: {error}", built.display()))?;
    if !is_static_x86_64_elf(&data) {
        return Err(format!(
            "BusyBox output is not a static x86_64 ELF: {}",
            built.display()
        ));
    }

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "cannot create BusyBox output directory {}: {error}",
                parent.display()
            )
        })?;
    }
    fs::copy(&built, &output)
        .map_err(|error| format!("cannot copy BusyBox to {}: {error}", output.display()))?;
    make_executable(&output)?;
    if options.clean {
        fs::remove_dir_all(&build_dir).map_err(|error| {
            format!(
                "cannot clean BusyBox build directory {} after build: {error}",
                build_dir.display()
            )
        })?;
    }
    println!("Built static BusyBox: {}", output.display());
    Ok(())
}

fn parse_options() -> Result<Options, String> {
    let mut args = env::args_os().skip(1);
    let mut options = Options {
        source: None,
        build_dir: None,
        output: None,
        compiler: None,
        jobs: None,
        clean: false,
    };

    while let Some(argument) = args.next() {
        let argument = argument.to_string_lossy();
        match argument.as_ref() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--source" => options.source = Some(PathBuf::from(next_value("--source", &mut args)?)),
            "--build-dir" => {
                options.build_dir = Some(PathBuf::from(next_value("--build-dir", &mut args)?))
            }
            "--output" => options.output = Some(PathBuf::from(next_value("--output", &mut args)?)),
            "--cc" => options.compiler = Some(next_value("--cc", &mut args)?),
            "--jobs" => {
                let jobs = next_value("--jobs", &mut args)?
                    .to_string_lossy()
                    .parse::<usize>()
                    .map_err(|_| "--jobs must be a positive integer".to_string())?;
                if jobs == 0 {
                    return Err("--jobs must be a positive integer".to_string());
                }
                options.jobs = Some(jobs);
            }
            "--clean" => options.clean = true,
            other => return Err(format!("unknown option `{other}` (use --help for usage)")),
        }
    }
    Ok(options)
}

fn next_value<I>(name: &str, args: &mut I) -> Result<OsString, String>
where
    I: Iterator<Item = OsString>,
{
    args.next()
        .ok_or_else(|| format!("{name} requires a value"))
}

fn print_help() {
    println!(
        "Build a static x86_64 BusyBox from toluene/busybox\n\n\
Usage: cargo run --manifest-path toluene/busybox-build/Cargo.toml -- [OPTIONS]\n\n\
Options:\n  \
--source PATH       BusyBox source directory\n  \
--build-dir PATH    out-of-tree build directory\n  \
--output PATH       output binary (default: target/busybox/busybox)\n  \
  --cc COMPILER       C compiler (default: musl-gcc, then gcc)\n  \
  --jobs N            parallel make jobs\n  \
  --clean             remove the out-of-tree build directory before and after building\n  \
-h, --help          show this help"
    );
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("busybox-build must live under the Fullerene workspace")
        .to_path_buf()
}

fn absolute_path(path: PathBuf, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
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
    compiler: &OsString,
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
    command.args(targets);
    let status = command
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
        ("CONFIG_PLATFORM_MINGW32", "n"),
        ("CONFIG_STATIC", "y"),
        ("CONFIG_STATIC_LIBGCC", "y"),
        ("CONFIG_PIE", "n"),
        ("CONFIG_FEATURE_PREFER_APPLETS", "y"),
        ("CONFIG_FEATURE_SH_STANDALONE", "y"),
        ("CONFIG_FEATURE_SH_EXTRA_QUIET", "y"),
        ("CONFIG_BUSYBOX_EXEC_PATH", "\"/bin/busybox\""),
        // busybox-w32's tc applet still expects removed Linux CBQ headers.
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

fn is_static_x86_64_elf(data: &[u8]) -> bool {
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
    let phoff = usize::try_from(u64::from_le_bytes(data[32..40].try_into().unwrap())).ok();
    let Some(phoff) = phoff else { return false };
    let phentsize = usize::from(u16::from_le_bytes([data[54], data[55]]));
    let phnum = usize::from(u16::from_le_bytes([data[56], data[57]]));
    if phentsize < 4 {
        return false;
    }
    for index in 0..phnum {
        let Some(offset) = phoff.checked_add(index.saturating_mul(phentsize)) else {
            return false;
        };
        let Some(entry) = data.get(offset..offset + 4) else {
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
