// fullerene/flasks/src/main.rs
use clap::{Parser, ValueEnum};
use isobemak::{BootInfo, IsoImage, UefiBootInfo, build_iso};
use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use env_logger;

#[derive(Parser)]
struct Args {
    /// Action to perform. When omitted, run the default x86_64 UEFI image.
    #[arg(value_enum, default_value_t = Action::Run)]
    command: Action,

    /// Clone the stable version of OVMF (edk2) into flasks/ovmf/edk2
    #[arg(long)]
    clone_ovmf: bool,

    /// Target CPU architecture.
    #[arg(long, value_enum, default_value_t = Arch::X86_64)]
    arch: Arch,

    /// Target platform. Defaults to pc-uefi for x86_64 and qemu-virt for AArch64.
    #[arg(long, value_enum)]
    platform: Option<Platform>,

    /// Run QEMU in headless mode (no GUI)
    #[arg(long)]
    headless: bool,

    /// Timeout for QEMU execution in seconds
    #[arg(long)]
    timeout: Option<u64>,

    /// Build fullerene.iso and exit without launching QEMU
    #[arg(long)]
    iso_only: bool,

    /// Use the unoptimized development profile for UEFI artifacts
    #[arg(long)]
    debug: bool,

    /// VGA device type: virtio-gpu, std, qxl, cirrus, none (default: virtio-gpu)
    #[arg(long, default_value = "virtio-gpu")]
    vga: String,

    /// Display backend: gtk, sdl, none, curses (default: gtk when not headless)
    #[arg(long)]
    display: Option<String>,

    /// Screen resolution in WxH format. Only effective with virtio-gpu/qxl.
    /// 1920x1080 keeps the desktop at the same density as the Photon quality
    /// reference instead of making every curve and glyph look pixel-doubled.
    #[arg(long, default_value = "1920x1080")]
    resolution: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Action {
    Build,
    Run,
    Debug,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Arch {
    #[value(name = "x86_64", alias = "x86-64", alias = "amd64")]
    X86_64,
    #[value(name = "aarch64", alias = "aa", alias = "arm64")]
    Aarch64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Platform {
    #[value(name = "pc-uefi")]
    PcUefi,
    #[value(name = "qemu-virt")]
    QemuVirt,
    Bramble,
}

impl Arch {
    fn rust_target(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64-unknown-uefi",
            Self::Aarch64 => "aarch64-unknown-none",
        }
    }

    fn default_platform(self) -> Platform {
        match self {
            Self::X86_64 => Platform::PcUefi,
            Self::Aarch64 => Platform::QemuVirt,
        }
    }

    fn kernel_artifact(self) -> &'static str {
        match self {
            Self::X86_64 => "fullerene-kernel.efi",
            Self::Aarch64 => "fullerene-kernel",
        }
    }

    fn cargo_package(self) -> &'static str {
        match self {
            Self::X86_64 => "fullerene-kernel",
            Self::Aarch64 => "fullerene-kernel-aarch64",
        }
    }
}

impl Platform {
    fn qemu_binary(self) -> &'static str {
        match self {
            Self::PcUefi => "qemu-system-x86_64",
            Self::QemuVirt | Self::Bramble => "qemu-system-aarch64",
        }
    }

    fn qemu_machine(self) -> &'static str {
        match self {
            Self::PcUefi => "q35,usb=off,pcspk-audiodev=speaker",
            Self::QemuVirt => "virt",
            Self::Bramble => "bramble",
        }
    }

    fn qemu_cpu(self) -> &'static str {
        match self {
            Self::PcUefi => "qemu64,+smap,+invtsc",
            Self::QemuVirt => "cortex-a72",
            Self::Bramble => "cortex-a72",
        }
    }

    fn validate(self, arch: Arch) -> io::Result<()> {
        let valid_pair = matches!(
            (arch, self),
            (Arch::X86_64, Self::PcUefi)
                | (Arch::Aarch64, Self::QemuVirt)
                | (Arch::Aarch64, Self::Bramble)
        );
        if !valid_pair {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("platform {:?} is not available for {:?}", self, arch),
            ));
        }
        if self == Self::Bramble {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the bramble platform is reserved for a future hardware backend",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Target {
    arch: Arch,
    platform: Platform,
}

impl Target {
    fn from_args(args: &Args) -> io::Result<Self> {
        let platform = args
            .platform
            .unwrap_or_else(|| args.arch.default_platform());
        platform.validate(args.arch)?;
        Ok(Self {
            arch: args.arch,
            platform,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BuildProfile {
    Release,
    Debug,
}

impl BuildProfile {
    fn from_debug(debug: bool) -> Self {
        if debug { Self::Debug } else { Self::Release }
    }

    fn cargo_name(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Debug => "dev",
        }
    }

    fn artifact_directory(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Debug => "debug",
        }
    }
}

fn main() -> io::Result<()> {
    // Initialize env_logger - it will respect RUST_LOG environment variable for filtering
    env_logger::init();
    let args = Args::parse();
    let target = Target::from_args(&args)?;
    let profile = BuildProfile::from_debug(args.debug || args.command == Action::Debug);
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("Failed to get workspace root")
        .to_path_buf();

    if target.arch == Arch::Aarch64 {
        if args.clone_ovmf || args.iso_only {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "OVMF and ISO options are only available for the x86_64 pc-uefi platform",
            ));
        }

        let kernel_path = build_aarch64_kernel(&workspace_root, profile)?;
        if args.command == Action::Build {
            println!("AArch64 kernel built at {}", kernel_path.display());
            return Ok(());
        }

        run_aarch64_qemu(
            &kernel_path,
            target.platform,
            args.command == Action::Debug,
            args.timeout,
        )?;
        return Ok(());
    }

    if args.clone_ovmf {
        setup_ovmf(&workspace_root)?;
        return Ok(());
    }

    let firmware_override = prepare_default_iwlwifi_firmware(&workspace_root)?;
    let firmware_path = firmware_override.as_ref().map(|file| file.path());

    if args.command == Action::Build || args.iso_only {
        let iso_path = create_iso(&workspace_root, profile, false, firmware_path)?;
        println!("ISO rebuilt at {}", iso_path.display());
        return Ok(());
    }

    run_qemu(&workspace_root, &args, profile, firmware_path)?;
    Ok(())
}

fn build_aarch64_kernel(workspace_root: &Path, profile: BuildProfile) -> io::Result<PathBuf> {
    let target = Arch::Aarch64;
    let mut cargo = Command::new("cargo");
    cargo
        .current_dir(workspace_root)
        .args([
            "build",
            "-q",
            "--package",
            target.cargo_package(),
            "--bin",
            target.kernel_artifact(),
            "--target",
            target.rust_target(),
            "--profile",
            profile.cargo_name(),
        ])
        // Keep the linker choice in the architecture-specific build path. The
        // bare-metal target is shipped with Rust's lld linker, so this does not
        // require a host C cross-toolchain.
        .env("CARGO_TARGET_AARCH64_UNKNOWN_NONE_LINKER", "rust-lld");

    let status = cargo.status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "{} build failed; the AArch64 bootstrap kernel or its target toolchain is not ready",
            target.rust_target()
        )));
    }

    let artifact = workspace_root
        .join("target")
        .join(target.rust_target())
        .join(profile.artifact_directory())
        .join(target.kernel_artifact());
    if !artifact.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "AArch64 kernel artifact was not produced at {}; the kernel entry point/linker layout is not wired yet",
                artifact.display()
            ),
        ));
    }
    Ok(artifact)
}

fn aarch64_qemu_args(artifact: &Path, platform: Platform, debug: bool) -> io::Result<Vec<String>> {
    if platform != Platform::QemuVirt {
        platform.validate(Arch::Aarch64)?;
    }

    let mut args = vec![
        "-M".to_string(),
        platform.qemu_machine().to_string(),
        "-cpu".to_string(),
        platform.qemu_cpu().to_string(),
        "-m".to_string(),
        "1G".to_string(),
        "-smp".to_string(),
        "1".to_string(),
        "-nographic".to_string(),
        "-kernel".to_string(),
        artifact.display().to_string(),
        "-no-reboot".to_string(),
        "-no-shutdown".to_string(),
    ];
    if debug {
        args.extend(["-S".to_string(), "-s".to_string()]);
    }
    Ok(args)
}

fn run_aarch64_qemu(
    artifact: &Path,
    platform: Platform,
    debug: bool,
    timeout: Option<u64>,
) -> io::Result<()> {
    let mut qemu = Command::new(platform.qemu_binary());
    let qemu_args = aarch64_qemu_args(artifact, platform, debug)?;
    log::info!(
        "Starting {} for AArch64 kernel {}",
        platform.qemu_binary(),
        artifact.display()
    );
    qemu.args(&qemu_args);

    let mut child = qemu.spawn()?;
    if let Some(timeout_secs) = timeout {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        loop {
            match child.try_wait()? {
                Some(status) => {
                    if !status.success() {
                        return Err(io::Error::other("AArch64 QEMU execution failed"));
                    }
                    return Ok(());
                }
                None if std::time::Instant::now() >= deadline => {
                    child.kill()?;
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("AArch64 QEMU timed out after {timeout_secs} seconds"),
                    ));
                }
                None => std::thread::sleep(std::time::Duration::from_millis(100)),
            }
        }
    }

    let status = child.wait()?;
    if !status.success() {
        return Err(io::Error::other("AArch64 QEMU execution failed"));
    }
    Ok(())
}

const LINUX_TESTED_FIRMWARE_COMMIT: &str = "11b7607b738eceacdf32505cb77b8151602bff9b";
const LINUX_TESTED_FIRMWARE_PATH: &str = "iwlwifi-7265D-29.ucode";

/// Return the firmware path that should be passed to nested Cargo builds.
///
/// The tracked linux-firmware tip currently contains the older 9ef079ed
/// 7265D blob, while the Linux baseline used 29.4063824552.0 / f2390aa8.
/// That known-good blob is retained in the submodule history, so the default
/// ISO build extracts it to a temporary file without modifying the submodule.
/// An explicit environment override always wins and is inherited unchanged.
fn prepare_default_iwlwifi_firmware(
    workspace_root: &Path,
) -> io::Result<Option<tempfile::NamedTempFile>> {
    if let Some(path) = env::var_os("FULLERENE_IWLWIFI_7265D_FW") {
        log::info!(
            "Using explicit 7265D firmware override {}",
            Path::new(&path).display()
        );
        return Ok(None);
    }

    let submodule = workspace_root.join("bonder").join("iwlwifi");
    let mut git = Command::new("git");
    git.current_dir(&submodule)
        .args([
            "show",
            &format!(
                "{}:{}",
                LINUX_TESTED_FIRMWARE_COMMIT, LINUX_TESTED_FIRMWARE_PATH
            ),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = git.output()?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "Linux-tested 7265D firmware is unavailable in the linux-firmware submodule: {}",
                detail.trim()
            ),
        ));
    }

    let target_dir = workspace_root.join("target");
    fs::create_dir_all(&target_dir)?;
    let mut firmware = tempfile::NamedTempFile::new_in(target_dir)?;
    firmware.write_all(&output.stdout)?;
    firmware.as_file_mut().flush()?;
    log::info!(
        "Defaulting 7265D firmware to Linux-tested commit {} ({} bytes)",
        LINUX_TESTED_FIRMWARE_COMMIT,
        output.stdout.len()
    );
    Ok(Some(firmware))
}

fn setup_ovmf(workspace_root: &PathBuf) -> io::Result<()> {
    // 1. Clean up previous failed clone attempts if they exist
    let edk2_dir = workspace_root.join("flasks").join("ovmf").join("edk2");
    if edk2_dir.exists() {
        log::info!("Removing previous edk2 clone directory...");
        std::fs::remove_dir_all(edk2_dir)?;
    }

    // 2. Check if OVMF is installed.
    let src_code = PathBuf::from("/usr/share/OVMF/OVMF_CODE.fd");
    let src_vars = PathBuf::from("/usr/share/OVMF/OVMF_VARS.fd");
    if !src_code.exists() || !src_vars.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "OVMF binaries not found in /usr/share/OVMF/. Please install the 'ovmf' package manually (e.g., 'sudo apt-get install -y ovmf' on Debian/Ubuntu).",
        ));
    }
    log::info!("OVMF binaries found.");

    // 3. Copy .fd files to flasks/ovmf/
    let dst_code = workspace_root
        .join("flasks")
        .join("ovmf")
        .join("RELEASEX64_OVMF_CODE.fd");
    let dst_vars = workspace_root
        .join("flasks")
        .join("ovmf")
        .join("RELEASEX64_OVMF_VARS.fd");

    log::info!(
        "Copying OVMF binaries to {}...",
        workspace_root.join("flasks").join("ovmf").display()
    );
    std::fs::copy(&src_code, &dst_code)?;
    std::fs::copy(&src_vars, &dst_vars)?;

    log::info!("OVMF setup completed successfully.");
    Ok(())
}

/// Build a UEFI package with cargo, using consistent target and profile settings.
fn build_uefi_package(
    workspace_root: &PathBuf,
    package: &str,
    features: Option<&str>,
    profile: BuildProfile,
    qemu_smoke_exit: bool,
    firmware_path: Option<&Path>,
) -> io::Result<()> {
    let mut args: Vec<&str> = vec![
        "build",
        "-q",
        "-Zbuild-std=core,alloc",
        "--package",
        package,
        "--target",
        "x86_64-unknown-uefi",
        "--profile",
        profile.cargo_name(),
    ];
    if let Some(feats) = features {
        args.extend(["--features", feats]);
    }
    let mut cargo = Command::new("cargo");
    cargo
        .current_dir(workspace_root)
        .env_remove("FULLERENE_BUSYBOX_SMOKE_QEMU_EXIT");
    if qemu_smoke_exit && env::var_os("FULLERENE_BUSYBOX_SMOKE").is_some() {
        cargo.env("FULLERENE_BUSYBOX_SMOKE_QEMU_EXIT", "1");
    }
    if qemu_smoke_exit && env::var_os("FULLERENE_USB_XHCI_SMOKE").is_some() {
        cargo.env("FULLERENE_USB_XHCI_SMOKE", "1");
    }
    if let Some(path) = firmware_path {
        cargo.env("FULLERENE_IWLWIFI_7265D_FW", path);
    }
    let status = cargo.args(&args).status()?;
    if !status.success() {
        return Err(io::Error::other(format!("{} build failed", package)));
    }
    Ok(())
}

fn create_iso(
    workspace_root: &PathBuf,
    profile: BuildProfile,
    qemu_smoke_exit: bool,
    firmware_path: Option<&Path>,
) -> io::Result<PathBuf> {
    // --- 1. Build fullerene-kernel (no_std) ---
    build_uefi_package(
        workspace_root,
        "fullerene-kernel",
        None,
        profile,
        qemu_smoke_exit,
        firmware_path,
    )?;

    let target_dir = workspace_root
        .join("target")
        .join("x86_64-unknown-uefi")
        .join(profile.artifact_directory());
    let kernel_path = target_dir.join("fullerene-kernel.efi");
    log::info!(
        "Kernel EFI at {} (size: {})",
        kernel_path.display(),
        kernel_path.metadata()?.len()
    );

    // --- 2. Build bellows (no_std) ---
    // Pass kernel path via environment variable so build.rs can copy
    // it into OUT_DIR.  No source‑tree pollution.
    let bellows_path = target_dir.join("bellows.efi");

    let status = Command::new("cargo")
        .current_dir(workspace_root)
        .env("KERNEL_BIN_PATH", &kernel_path)
        .envs(firmware_path.map(|path| ("FULLERENE_IWLWIFI_7265D_FW", path)))
        .args([
            "build",
            "-q",
            "-Zbuild-std=core,alloc",
            "--package",
            "bellows",
            "--target",
            "x86_64-unknown-uefi",
            "--profile",
            profile.cargo_name(),
            "--features",
            "debug_loader",
        ])
        .status()?;
    if !status.success() {
        return Err(io::Error::other("bellows build failed"));
    }

    // --- 3. Create ISO using isobemak ---
    let iso_path = workspace_root.join("fullerene.iso");

    let image = IsoImage {
        volume_id: None,
        // `UefiBootInfo` places both EFI payloads in the embedded ESP.  The
        // Bellows loader has an El Torito fallback for retaining installer
        // payloads, so duplicating them in ISO9660 is unnecessary.
        files: Vec::new(),
        boot_info: BootInfo {
            bios_boot: None,
            uefi_boot: Some(UefiBootInfo {
                boot_image: bellows_path.clone(),
                kernel_image: kernel_path.clone(),
                destination_in_iso: "EFI/BOOT/BOOTX64.EFI".to_string(),
                additional_efi_boot_files: Vec::new(),
                grub_cfg_content: None,
            }),
        },
        layout_profile: isobemak::IsoLayoutProfile::hardware(),
    };
    let (_iso_output_path, _temp_fat_holder, _iso_file, _logical_fat_size) =
        build_iso(&iso_path, &image, true)?;

    Ok(iso_path)
}

fn create_iso_and_setup(
    workspace_root: &PathBuf,
    profile: BuildProfile,
    firmware_path: Option<&Path>,
) -> io::Result<(PathBuf, PathBuf, PathBuf, tempfile::NamedTempFile)> {
    let iso_path = create_iso(workspace_root, profile, true, firmware_path)?;

    let ovmf_fd_path = workspace_root
        .join("flasks")
        .join("ovmf")
        .join("RELEASEX64_OVMF_CODE.fd");
    let ovmf_vars_fd_original_path = workspace_root
        .join("flasks")
        .join("ovmf")
        .join("RELEASEX64_OVMF_VARS.fd");

    // Create a temporary file for OVMF_VARS.fd to ensure a clean state each run
    let mut temp_ovmf_vars_fd = tempfile::NamedTempFile::new()?;
    std::io::copy(
        &mut std::fs::File::open(&ovmf_vars_fd_original_path)?,
        temp_ovmf_vars_fd.as_file_mut(),
    )?;
    let ovmf_vars_fd_path = temp_ovmf_vars_fd.path().to_path_buf();

    Ok((iso_path, ovmf_fd_path, ovmf_vars_fd_path, temp_ovmf_vars_fd))
}

fn run_qemu(
    workspace_root: &PathBuf,
    args: &Args,
    profile: BuildProfile,
    firmware_path: Option<&Path>,
) -> io::Result<()> {
    log::info!("Starting QEMU...");
    let (iso_path, ovmf_fd_path, ovmf_vars_fd_path, temp_ovmf_vars_fd) =
        create_iso_and_setup(&workspace_root, profile, firmware_path)?;

    // --- 4. Run QEMU with the created ISO ---

    let ovmf_fd_drive = format!(
        "if=pflash,format=raw,unit=0,readonly=on,file={}",
        ovmf_fd_path.display()
    );
    let ovmf_vars_fd_drive = format!(
        "if=pflash,format=raw,unit=1,file={}",
        ovmf_vars_fd_path.display()
    );

    let iso_path_str = iso_path.to_str().expect("ISO path should be valid UTF-8");

    let mut qemu_cmd = Command::new(Platform::PcUefi.qemu_binary());
    let mut qemu_args: Vec<String> = vec![
        "-m".to_string(),
        "4G".to_string(),
        "-cpu".to_string(),
        Platform::PcUefi.qemu_cpu().to_string(),
        "-smp".to_string(),
        "1".to_string(),
        "-M".to_string(),
        Platform::PcUefi.qemu_machine().to_string(),
    ];

    // --- VGA device (dynamic) ---
    match args.vga.as_str() {
        "virtio-gpu" => {
            qemu_args.push("-vga".to_string());
            qemu_args.push("none".to_string());
            // Parse resolution
            let res_parts: Vec<&str> = args.resolution.split('x').collect();
            let (w, h) = if res_parts.len() == 2 {
                (res_parts[0], res_parts[1])
            } else {
                ("1920", "1080")
            };
            qemu_args.push("-device".to_string());
            qemu_args.push(format!(
                "virtio-gpu-pci,disable-legacy=on,disable-modern=off,xres={},yres={}",
                w, h
            ));
        }
        "std" => {
            qemu_args.push("-vga".to_string());
            qemu_args.push("std".to_string());
        }
        "qxl" => {
            qemu_args.push("-vga".to_string());
            qemu_args.push("qxl".to_string());
        }
        "cirrus" => {
            qemu_args.push("-vga".to_string());
            qemu_args.push("cirrus".to_string());
        }
        "none" => {
            qemu_args.push("-vga".to_string());
            qemu_args.push("none".to_string());
        }
        other => {
            log::warn!("Unknown VGA type '{}', falling back to virtio-gpu", other);
            qemu_args.push("-vga".to_string());
            qemu_args.push("none".to_string());
            qemu_args.push("-device".to_string());
            qemu_args.push("virtio-gpu-pci,disable-legacy=on,disable-modern=off".to_string());
        }
    }

    // --- Display backend (dynamic) ---
    // Default to SDL because GTK creates a USB tablet that captures all
    // mouse events and prevents PS/2 i8042 AUX port from receiving data.
    // SDL routes mouse events through PS/2 by default.
    let display = args
        .display
        .as_deref()
        .unwrap_or(if args.headless { "none" } else { "sdl" });
    qemu_args.push("-display".to_string());
    match display {
        "gtk" => {
            qemu_args.push("gtk,gl=off,window-close=on,zoom-to-fit=on,grab-on-hover=on".to_string())
        }
        "sdl" => qemu_args.push("sdl,gl=off".to_string()),
        "none" => qemu_args.push("none".to_string()),
        "curses" => qemu_args.push("curses".to_string()),
        other => {
            log::warn!("Unknown display backend '{}', using none", other);
            qemu_args.push("none".to_string());
        }
    }

    let qemu_accel =
        env::var("FULLERENE_QEMU_ACCEL").unwrap_or_else(|_| "tcg,thread=single".to_string());
    qemu_args.extend([
        "-serial".to_string(),
        "stdio".to_string(),
        "-accel".to_string(),
        qemu_accel,
        "-d".to_string(),
        "int,cpu_reset,guest_errors,unimp".to_string(),
        "-D".to_string(),
        "qemu_log.txt".to_string(),
        "-monitor".to_string(),
        "none".to_string(),
    ]);

    qemu_args.push("-drive".to_string());
    qemu_args.push(ovmf_fd_drive);
    qemu_args.push("-drive".to_string());
    qemu_args.push(ovmf_vars_fd_drive);
    qemu_args.push("-drive".to_string());
    qemu_args.push(format!(
        "file={},media=cdrom,if=ide,format=raw",
        iso_path_str
    ));

    if env::var_os("FULLERENE_USB_XHCI_SMOKE").is_some() {
        // QEMU's qemu-xhci needs an actual mass-storage backend. A sparse
        // image is enough for BOT INQUIRY/READ CAPACITY and keeps the smoke
        // test independent of host USB hardware and filesystem contents.
        let usb_image_path = workspace_root
            .join("target")
            .join("qemu")
            .join("usb-xhci-smoke.img");
        if let Some(parent) = usb_image_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let usb_image = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&usb_image_path)?;
        usb_image.set_len(16 * 1024 * 1024)?;

        qemu_args.extend([
            "-drive".to_string(),
            format!(
                "file={},if=none,format=raw,id=usb_xhci_disk,readonly=on",
                usb_image_path.display()
            ),
            "-device".to_string(),
            // Expose one SuperSpeed root port. QEMU otherwise creates eight
            // ports; the guest's bounded root-port scan should not be
            // coupled to unused emulated PHYs.
            "qemu-xhci,id=xhci,p2=1,p3=1".to_string(),
            "-device".to_string(),
            "usb-storage,bus=xhci.0,port=1,drive=usb_xhci_disk".to_string(),
        ]);
    }

    qemu_args.extend([
        "-no-reboot".to_string(),
        "-no-shutdown".to_string(),
        "-device".to_string(),
        "isa-debug-exit,iobase=0xf4,iosize=0x04".to_string(),
        "-rtc".to_string(),
        "base=utc".to_string(),
        "-boot".to_string(),
        "menu=on,order=d".to_string(),
        // ── PC Speaker audio (audiodev for PulseAudio) ───
        "-audiodev".to_string(),
        "pa,id=speaker,out.mixing-engine=off".to_string(),
        // ── HD Audio device (Intel HDA) ───
        "-audiodev".to_string(),
        "pa,id=hda,timer-period=1000,out.mixing-engine=off".to_string(),
        "-device".to_string(),
        "intel-hda,debug=0".to_string(),
        "-device".to_string(),
        "hda-duplex,audiodev=hda".to_string(),
    ]);

    if args.command == Action::Debug {
        // Match the AArch64 debug action: pause at reset and expose the GDB
        // stub on the conventional port.
        qemu_args.extend(["-S".to_string(), "-s".to_string()]);
    }

    qemu_cmd.args(&qemu_args);

    // Keep the temporary file alive until QEMU exits
    let _temp_ovmf_vars_fd_holder = temp_ovmf_vars_fd;
    // LD_PRELOAD is a workaround for specific QEMU/libpthread versions.
    // It can be overridden by setting the FULLERENE_QEMU_LD_PRELOAD environment variable.
    let ld_preload_path = env::var("FULLERENE_QEMU_LD_PRELOAD").unwrap_or_else(|_| {
        flasks::find_libpthread().expect("libpthread.so.0 not found in common locations")
    });
    qemu_cmd.env("LD_PRELOAD", ld_preload_path);

    let mut child = qemu_cmd.spawn()?;
    let debug_exit_smoke_requested = env::var_os("FULLERENE_LINUX_MUSL_SMOKE").is_some()
        || env::var_os("FULLERENE_BUSYBOX_SMOKE").is_some()
        || env::var_os("FULLERENE_IPC_KERNEL_SMOKE").is_some()
        || env::var_os("FULLERENE_USB_XHCI_SMOKE").is_some();
    let qemu_status_is_valid = |status: &std::process::ExitStatus| {
        if debug_exit_smoke_requested {
            status.code() == Some(35)
        } else {
            status.success()
        }
    };

    if let Some(timeout_secs) = args.timeout {
        let timeout_duration = std::time::Duration::from_secs(timeout_secs);
        let timeout_handle = std::thread::spawn(move || {
            std::thread::sleep(timeout_duration);
        });

        // We need to wait for either the child to exit or the timeout thread to finish
        // Since we can't easily "select" on a process, we'll poll the child
        loop {
            match child.try_wait()? {
                Some(status) => {
                    if !qemu_status_is_valid(&status) {
                        return Err(io::Error::other("QEMU execution failed"));
                    }
                    return Ok(());
                }
                None => {
                    if timeout_handle.is_finished() {
                        log::warn!(
                            "QEMU timed out after {} seconds. Killing process...",
                            timeout_secs
                        );
                        child.kill()?;
                        return Err(io::Error::other("QEMU execution timed out"));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
    } else {
        let qemu_status = child.wait()?;
        if !qemu_status_is_valid(&qemu_status) {
            return Err(io::Error::other("QEMU execution failed"));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Action, Arch, Args, BuildProfile, Platform, aarch64_qemu_args};
    use clap::Parser;
    use std::path::Path;

    #[test]
    fn release_is_the_default_profile() {
        let profile = BuildProfile::from_debug(false);
        assert_eq!(profile.cargo_name(), "release");
        assert_eq!(profile.artifact_directory(), "release");
    }

    #[test]
    fn debug_profile_keeps_dev_artifact_layout() {
        let profile = BuildProfile::from_debug(true);
        assert_eq!(profile.cargo_name(), "dev");
        assert_eq!(profile.artifact_directory(), "debug");
    }

    #[test]
    fn aarch64_run_selects_qemu_virt_defaults() {
        let args = Args::try_parse_from(["flasks", "run", "--arch", "aarch64"]).unwrap();
        let target = super::Target::from_args(&args).unwrap();
        assert_eq!(args.command, Action::Run);
        assert_eq!(target.arch, Arch::Aarch64);
        assert_eq!(target.platform, Platform::QemuVirt);
        assert_eq!(target.arch.rust_target(), "aarch64-unknown-none");
    }

    #[test]
    fn aa_is_an_aarch64_alias() {
        let args = Args::try_parse_from(["flasks", "run", "--arch", "aa"]).unwrap();
        assert_eq!(args.arch, Arch::Aarch64);
    }

    #[test]
    fn aarch64_qemu_command_uses_virt_and_kernel_artifact() {
        let args = aarch64_qemu_args(
            Path::new("target/aarch64-unknown-none/release/fullerene-kernel"),
            Platform::QemuVirt,
            false,
        )
        .unwrap();
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "-M" && pair[1] == "virt")
        );
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "-cpu" && pair[1] == "cortex-a72")
        );
        assert!(args.iter().any(|arg| arg == "-nographic"));
        assert!(args.iter().any(|arg| arg.ends_with("fullerene-kernel")));
    }

    #[test]
    fn bramble_is_reserved_without_being_aliased_to_aarch64_qemu() {
        let args = Args::try_parse_from([
            "flasks",
            "run",
            "--arch",
            "aarch64",
            "--platform",
            "bramble",
        ])
        .unwrap();
        let error = super::Target::from_args(&args).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    }
}
