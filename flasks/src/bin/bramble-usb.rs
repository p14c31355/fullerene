//! Rust replacement for the Bramble USB handoff shell harness.
//!
//! The harness deliberately delegates image construction and the actual
//! Fastboot protocol to Flasks, so the safety boundary stays in one place:
//! the only device-side image operation is `fastboot boot`.

use clap::{Parser, Subcommand, ValueEnum};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const DEFAULT_SERIAL: &str = "26191JECB00076";
const DEFAULT_TEMPLATE: &str = "/tmp/fullerene-stock-template.Uvg3m2/boot.img";
const BOOTLOADER_USB: &str = "18d1:4ee0";
const ANDROID_FALLBACK_USB: &str = "18d1:4ee7";
const FULLERENE_USB: &str = "1234:0001";
const RECOVERY_TIMEOUT_SECS: u64 = 75;

#[derive(Parser, Debug)]
#[command(about = "Run non-destructive Bramble USB handoff experiments")]
struct Args {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand, Debug)]
enum CommandKind {
    /// Build, audit, RAM-boot, and verify one Bramble USB handoff.
    Loop(LoopArgs),
    /// Try bounded platform-route variants in sequence.
    Matrix(MatrixArgs),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Route {
    Power,
    Typec,
    #[value(name = "typec-role")]
    TypecRole,
    Pdc,
    Smmu,
}

impl Route {
    fn as_str(self) -> &'static str {
        match self {
            Self::Power => "power",
            Self::Typec => "typec",
            Self::TypecRole => "typec-role",
            Self::Pdc => "pdc",
            Self::Smmu => "smmu",
        }
    }
}

#[derive(Parser, Debug, Clone)]
struct LoopArgs {
    #[arg(long, default_value = DEFAULT_SERIAL)]
    serial: String,
    #[arg(long, default_value = DEFAULT_TEMPLATE)]
    template: PathBuf,
    #[arg(long, default_value_t = 30)]
    enum_timeout: u64,
    #[arg(long, default_value_t = 30)]
    hold: u64,
    #[arg(long, default_value_t = 30)]
    fastboot_wait: u64,
    #[arg(long)]
    irq_route: Option<Route>,
    #[arg(long)]
    super_speed: bool,
    #[arg(long)]
    normal: bool,
    #[arg(long)]
    pullup_only: bool,
    #[arg(long)]
    no_smmu: bool,
    #[arg(long)]
    no_core_reset: bool,
    #[arg(long)]
    uncompressed: bool,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Parser, Debug)]
struct MatrixArgs {
    /// Restrict the matrix; repeat this option to choose several routes.
    #[arg(long = "route")]
    routes: Vec<Route>,
    #[arg(long, default_value = DEFAULT_SERIAL)]
    serial: String,
    #[arg(long, default_value = DEFAULT_TEMPLATE)]
    template: PathBuf,
    #[arg(long, default_value_t = 30)]
    enum_timeout: u64,
    #[arg(long, default_value_t = 30)]
    hold: u64,
    #[arg(long, default_value_t = 30)]
    fastboot_wait: u64,
    #[arg(long)]
    super_speed: bool,
    #[arg(long)]
    no_smmu: bool,
    #[arg(long)]
    no_core_reset: bool,
    #[arg(long)]
    dry_run: bool,
}

struct JournalGuard {
    child: Option<Child>,
    run_dir: PathBuf,
    start_iso: String,
}

impl JournalGuard {
    fn start(run_dir: &Path) -> io::Result<Self> {
        let start_iso = command_text("date", &["--iso-8601=seconds"])?
            .trim()
            .to_owned();
        let log = File::create(run_dir.join("kernel.log"))?;
        let child = Command::new("journalctl")
            .args(["-kf", "-o", "short-iso", "--since", "now", "--no-pager"])
            .stdout(Stdio::from(log))
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self {
            child: Some(child),
            run_dir: run_dir.to_owned(),
            start_iso,
        })
    }

    fn save_final(&self) {
        let output = Command::new("journalctl")
            .args(["-k", "--since", &self.start_iso, "--no-pager"])
            .output();
        if let Ok(output) = output {
            let _ = fs::write(self.run_dir.join("kernel-final.log"), output.stdout);
        }
    }
}

impl Drop for JournalGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn main() -> io::Result<()> {
    let args = Args::parse();
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("flasks has a workspace parent")
        .to_owned();
    match args.command {
        CommandKind::Loop(args) => run_loop(&workspace, args),
        CommandKind::Matrix(args) => run_matrix(&workspace, args),
    }
}

fn run_matrix(workspace: &Path, args: MatrixArgs) -> io::Result<()> {
    let routes = if args.routes.is_empty() {
        vec![
            Route::Power,
            Route::Typec,
            Route::TypecRole,
            Route::Pdc,
            Route::Smmu,
        ]
    } else {
        args.routes.clone()
    };
    println!(
        "Bramble USB route matrix: {}",
        routes
            .iter()
            .map(|route| route.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    );
    if args.dry_run {
        for route in routes {
            let loop_args = loop_args_for_route(&args, route);
            print_loop_command(&loop_args);
        }
        return Ok(());
    }

    let run_dir = create_run_dir("fullerene-bramble-matrix")?;
    println!("Matrix logs: {}", run_dir.display());
    for route in routes {
        println!("=== route: {} ===", route.as_str());
        let loop_args = loop_args_for_route(&args, route);
        match run_loop_with_dir(workspace, loop_args, Some(&run_dir)) {
            Ok(()) => {
                println!("USB route matrix: PASS ({})", route.as_str());
                return Ok(());
            }
            Err(error) => {
                eprintln!("route {} failed: {error}", route.as_str());
                eprintln!("trying the next route only if Fastboot recovered");
            }
        }
    }
    Err(io::Error::other(format!(
        "USB route matrix failed; logs are under {}",
        run_dir.display()
    )))
}

fn loop_args_for_route(args: &MatrixArgs, route: Route) -> LoopArgs {
    LoopArgs {
        serial: args.serial.clone(),
        template: args.template.clone(),
        enum_timeout: args.enum_timeout,
        hold: args.hold,
        fastboot_wait: args.fastboot_wait,
        irq_route: Some(route),
        super_speed: args.super_speed,
        normal: false,
        pullup_only: false,
        no_smmu: args.no_smmu,
        no_core_reset: args.no_core_reset,
        uncompressed: false,
        dry_run: false,
    }
}

fn run_loop(workspace: &Path, args: LoopArgs) -> io::Result<()> {
    if args.normal && (args.super_speed || args.pullup_only) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--normal cannot be combined with --super-speed or --pullup-only",
        ));
    }
    if args.pullup_only && (args.no_smmu || args.no_core_reset || args.irq_route.is_some()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--pullup-only cannot be combined with IRQ, SMMU, or core-reset differentials",
        ));
    }
    if args.no_smmu && args.normal {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--no-smmu requires the gadget handoff probe",
        ));
    }
    if args.no_core_reset && args.normal {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--no-core-reset requires the gadget handoff probe",
        ));
    }
    if !args.template.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("stock boot template not found: {}", args.template.display()),
        ));
    }
    if args.dry_run {
        print_loop_command(&args);
        return Ok(());
    }
    run_loop_with_dir(workspace, args, None)
}

fn run_loop_with_dir(
    workspace: &Path,
    args: LoopArgs,
    matrix_dir: Option<&Path>,
) -> io::Result<()> {
    let run_dir = match matrix_dir {
        Some(matrix_dir) => {
            create_child_run_dir(matrix_dir, args.irq_route.map_or("loop", Route::as_str))?
        }
        None => create_run_dir("fullerene-bramble-loop")?,
    };
    let output = run_dir.join("fullerene-bramble-boot.img");
    println!("Bramble serial: {}", args.serial);
    println!("Stock template: {}", args.template.display());
    println!("Boot artifact: {}", output.display());
    println!("Logs: {}", run_dir.display());

    wait_for_fastboot(&args.serial, args.fastboot_wait)?;
    let product = fastboot_getvar(&args.serial, "product")?;
    if !product
        .lines()
        .any(|line| line.to_ascii_lowercase().contains("product: bramble"))
    {
        return Err(io::Error::other(format!(
            "unexpected Fastboot product (expected bramble):\n{product}"
        )));
    }
    fs::write(run_dir.join("fastboot-getvar-product.txt"), &product)?;
    let getvar = run_capture(
        &run_dir.join("fastboot-getvar-before.txt"),
        &mut fastboot_command(&args.serial, &["getvar", "all"]),
    )?;
    if !getvar.status.success() {
        return Err(io::Error::other("Fastboot getvar all failed before boot"));
    }
    let _ = capture_simple(&run_dir, "fastboot-usb-tree", "lsusb", &["-t"]);

    let journal = JournalGuard::start(&run_dir)?;
    let build = build_command(workspace, &args, &output);
    let build_output = run_capture(&run_dir.join("build.log"), &mut build_command_owned(build))?;
    if !build_output.status.success() || !output.is_file() {
        journal.save_final();
        return Err(io::Error::other("build/audit failed"));
    }
    let sha = sha256(&output)?;
    fs::write(
        run_dir.join("artifact.sha256"),
        format!("{sha}  {}\n", output.display()),
    )?;

    let boot = boot_command(workspace, &output);
    let boot_output = run_capture(&run_dir.join("boot.log"), &mut build_command_owned(boot))?;
    if !boot_output.status.success() {
        journal.save_final();
        return Err(io::Error::other("Fastboot boot failed"));
    }

    wait_until_absent(BOOTLOADER_USB, 15);
    let deadline = Instant::now() + Duration::from_secs(args.enum_timeout);
    let mut android_fallback = false;
    while Instant::now() < deadline {
        if usb_present(FULLERENE_USB) {
            println!("Fullerene USB enumeration: PASS");
            let descriptor =
                capture_simple(&run_dir, "lsusb-v", "lsusb", &["-d", FULLERENE_USB, "-v"])?;
            if !descriptor.status.success() {
                journal.save_final();
                return Err(io::Error::other("Fullerene descriptor read failed"));
            }
            let _ = capture_simple(&run_dir, "lsusb-tree", "lsusb", &["-t"]);
            if args.super_speed && !has_superspeed_link(&run_dir)? {
                journal.save_final();
                return Err(io::Error::other("Fullerene USB has no SuperSpeed link"));
            }
            let hold_deadline = Instant::now() + Duration::from_secs(args.hold);
            while Instant::now() < hold_deadline {
                if !usb_present(FULLERENE_USB) {
                    journal.save_final();
                    return Err(io::Error::other("Fullerene USB disappeared during hold"));
                }
                thread::sleep(Duration::from_secs(1));
            }
            journal.save_final();
            println!("Fullerene USB handoff and hold verification: PASS");
            return Ok(());
        }
        if usb_present(ANDROID_FALLBACK_USB) {
            android_fallback = true;
            break;
        }
        thread::sleep(Duration::from_secs(1));
    }

    if android_fallback {
        let _ = capture_simple(
            &run_dir,
            "android-fallback-usb",
            "lsusb",
            &["-d", ANDROID_FALLBACK_USB, "-v"],
        );
        let _ = capture_simple(
            &run_dir,
            "adb-devices",
            "adb",
            &["-s", &args.serial, "devices", "-l"],
        );
        let _ = capture_simple(
            &run_dir,
            "adb-state",
            "adb",
            &["-s", &args.serial, "get-state"],
        );
    } else {
        println!(
            "Fullerene USB did not enumerate; waiting up to {RECOVERY_TIMEOUT_SECS}s for probe recovery"
        );
        let recovery_deadline = Instant::now() + Duration::from_secs(RECOVERY_TIMEOUT_SECS);
        while Instant::now() < recovery_deadline {
            if usb_present(ANDROID_FALLBACK_USB) {
                android_fallback = true;
                let _ = capture_simple(
                    &run_dir,
                    "android-fallback-usb",
                    "lsusb",
                    &["-d", ANDROID_FALLBACK_USB, "-v"],
                );
                let _ = capture_simple(
                    &run_dir,
                    "adb-devices",
                    "adb",
                    &["-s", &args.serial, "devices", "-l"],
                );
                let _ = capture_simple(
                    &run_dir,
                    "adb-state",
                    "adb",
                    &["-s", &args.serial, "get-state"],
                );
                break;
            }
            if usb_present(BOOTLOADER_USB) {
                if let Ok(output) = fastboot_command(&args.serial, &["getvar", "all"]).output() {
                    let mut file = File::create(run_dir.join("fastboot-getvar-after.txt"))?;
                    file.write_all(&output.stdout)?;
                    file.write_all(&output.stderr)?;
                }
                journal.save_final();
                return Err(io::Error::other(format!(
                    "Fullerene USB enumeration timeout; probe recovered to Fastboot {BOOTLOADER_USB}; logs: {}",
                    run_dir.display()
                )));
            }
            thread::sleep(Duration::from_secs(1));
        }
    }
    let message = if android_fallback {
        format!(
            "Fullerene USB enumeration timeout; stock Android fallback {ANDROID_FALLBACK_USB} detected"
        )
    } else {
        format!("Fullerene USB enumeration timeout; expected {FULLERENE_USB}")
    };
    journal.save_final();
    Err(io::Error::other(format!(
        "{message}; logs: {}",
        run_dir.display()
    )))
}

fn print_loop_command(args: &LoopArgs) {
    println!("Rust Bramble USB loop (dry-run)");
    println!("serial={}", args.serial);
    println!("template={}", args.template.display());
    println!("mode={}", mode_name(args));
    if let Some(route) = args.irq_route {
        println!("irq-route={}", route.as_str());
    }
    println!("operation=fastboot boot only");
}

fn mode_name(args: &LoopArgs) -> &'static str {
    if args.normal {
        "normal"
    } else if args.pullup_only {
        "usb-pullup-probe"
    } else if args.super_speed {
        "usb-gadget-handoff-super-speed-probe"
    } else {
        "usb-gadget-handoff-probe"
    }
}

fn build_command(workspace: &Path, args: &LoopArgs, output: &Path) -> CommandSpec {
    let mut arguments = vec![
        "run".to_owned(),
        "-q".to_owned(),
        "-p".to_owned(),
        "flasks".to_owned(),
        "--".to_owned(),
        "build".to_owned(),
        "--arch".to_owned(),
        "aarch64".to_owned(),
        "--platform".to_owned(),
        "bramble".to_owned(),
    ];
    if !args.normal {
        arguments.push(if args.pullup_only {
            "--usb-pullup-probe".to_owned()
        } else if args.super_speed {
            "--usb-gadget-handoff-super-speed-probe".to_owned()
        } else {
            "--usb-gadget-handoff-probe".to_owned()
        });
    }
    arguments.extend([
        "--boot-template".to_owned(),
        args.template.display().to_string(),
        "--boot-output".to_owned(),
        output.display().to_string(),
        "--qemu-preflight".to_owned(),
    ]);
    if args.uncompressed {
        arguments.push("--boot-uncompressed".to_owned());
    }
    if args.no_smmu {
        arguments.push("--usb-gadget-handoff-no-smmu".to_owned());
    }
    let mut envs = Vec::new();
    if let Some(route) = args.irq_route {
        envs.push((
            "FULLERENE_AARCH64_USB_PROBE_IRQ_ROUTES".to_owned(),
            route.as_str().to_owned(),
        ));
    }
    if args.no_core_reset {
        envs.push((
            "FULLERENE_AARCH64_USB_GADGET_HANDOFF_PRESERVE_CORE".to_owned(),
            "1".to_owned(),
        ));
    }
    CommandSpec {
        program: "cargo".to_owned(),
        arguments,
        envs,
        current_dir: workspace.to_owned(),
    }
}

fn boot_command(workspace: &Path, image: &Path) -> CommandSpec {
    CommandSpec {
        program: "cargo".to_owned(),
        arguments: vec![
            "run".to_owned(),
            "-q".to_owned(),
            "-p".to_owned(),
            "flasks".to_owned(),
            "--".to_owned(),
            "boot".to_owned(),
            "--arch".to_owned(),
            "aarch64".to_owned(),
            "--platform".to_owned(),
            "bramble".to_owned(),
            image.display().to_string(),
        ],
        envs: Vec::new(),
        current_dir: workspace.to_owned(),
    }
}

#[derive(Debug)]
struct CommandSpec {
    program: String,
    arguments: Vec<String>,
    envs: Vec<(String, String)>,
    current_dir: PathBuf,
}

fn build_command_owned(spec: CommandSpec) -> Command {
    command_from_spec(&spec)
}

fn command_from_spec(spec: &CommandSpec) -> Command {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.arguments)
        .envs(spec.envs.iter().map(|(key, value)| (key, value)))
        .current_dir(&spec.current_dir);
    command
}

fn fastboot_command(serial: &str, arguments: &[&str]) -> Command {
    let mut command = Command::new("fastboot");
    command.arg("-s").arg(serial).args(arguments);
    command
}

fn wait_for_fastboot(serial: &str, timeout_secs: u64) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        let output = Command::new("fastboot").args(["devices", "-l"]).output();
        if let Ok(output) = output {
            let text = String::from_utf8_lossy(&output.stdout);
            if text.lines().any(|line| line.starts_with(serial)) {
                return Ok(());
            }
        }
        thread::sleep(Duration::from_secs(1));
    }
    Err(io::Error::other(format!(
        "device {serial} is not available in Fastboot"
    )))
}

fn fastboot_getvar(serial: &str, variable: &str) -> io::Result<String> {
    let output = fastboot_command(serial, &["getvar", variable]).output()?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(text)
}

fn run_capture(log_path: &Path, command: &mut Command) -> io::Result<Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let output = command.output()?;
    print_bytes(&output.stdout);
    print_bytes(&output.stderr);
    let mut file = File::create(log_path)?;
    file.write_all(&output.stdout)?;
    file.write_all(&output.stderr)?;
    Ok(output)
}

fn capture_simple(
    run_dir: &Path,
    label: &str,
    program: &str,
    arguments: &[&str],
) -> io::Result<Output> {
    let output = Command::new(program).args(arguments).output()?;
    let mut file = File::create(run_dir.join(format!("{label}.txt")))?;
    file.write_all(&output.stdout)?;
    file.write_all(&output.stderr)?;
    Ok(output)
}

fn print_bytes(bytes: &[u8]) {
    let _ = io::stdout().write_all(bytes);
    let _ = io::stdout().flush();
}

fn command_text(program: &str, arguments: &[&str]) -> io::Result<String> {
    let output = Command::new(program).args(arguments).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!("{program} failed")));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn usb_present(identity: &str) -> bool {
    Command::new("lsusb")
        .args(["-d", identity])
        .output()
        .map(|output| output.status.success() && !output.stdout.is_empty())
        .unwrap_or(false)
}

fn wait_until_absent(identity: &str, timeout_secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline && usb_present(identity) {
        thread::sleep(Duration::from_secs(1));
    }
}

fn has_superspeed_link(run_dir: &Path) -> io::Result<bool> {
    let tree = fs::read_to_string(run_dir.join("lsusb-tree.txt"))?;
    Ok(tree
        .lines()
        .any(|line| line.contains("5000M") || line.contains("10000M")))
}

fn sha256(path: &Path) -> io::Result<String> {
    let output = Command::new("sha256sum").arg(path).output()?;
    if !output.status.success() {
        return Err(io::Error::other("sha256sum failed"));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned())
}

fn create_run_dir(prefix: &str) -> io::Result<PathBuf> {
    for suffix in 0..1000u32 {
        let path = std::env::temp_dir().join(format!("{prefix}.{}.{}", std::process::id(), suffix));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other(
        "could not create a unique temporary run directory",
    ))
}

fn create_child_run_dir(parent: &Path, name: &str) -> io::Result<PathBuf> {
    let path = parent.join(name);
    fs::create_dir(&path)?;
    Ok(path)
}
