//! Command-line entry point for the shared BusyBox build workflow.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use busybox_build::{BuildOptions, build};

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
            .unwrap_or_else(|| workspace.join("target/busybox-build")),
        &workspace,
    );
    let output = absolute_path(
        options
            .output
            .unwrap_or_else(|| workspace.join("target/busybox/busybox")),
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
            .map(|count| count.get().min(8))
    });
    build(&BuildOptions {
        source,
        build_dir,
        output: output.clone(),
        compiler: options.compiler,
        jobs,
        clean: options.clean,
    })?;
    println!(
        "Built or reused dynamically linked glibc BusyBox: {}",
        output.display()
    );
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
            "--source" => options.source = Some(next_value("--source", &mut args)?.into()),
            "--build-dir" => options.build_dir = Some(next_value("--build-dir", &mut args)?.into()),
            "--output" => options.output = Some(next_value("--output", &mut args)?.into()),
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
        "Build a dynamically linked glibc x86_64 BusyBox from toluene/busybox\n\n\
Usage: cargo run --manifest-path toluene/busybox-build/Cargo.toml -- [OPTIONS]\n\n\
Options:\n  \
--source PATH       BusyBox source directory\n  \
--build-dir PATH    out-of-tree build directory\n  \
--output PATH       output binary (default: target/busybox/busybox)\n  \
--cc COMPILER       C compiler (default: FULLERENE_BUSYBOX_CC, then gcc)\n  \
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
