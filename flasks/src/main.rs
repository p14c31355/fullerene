// fullerene/flasks/src/main.rs
use clap::{Parser, ValueEnum};
use isobemak::{BootInfo, IsoImage, UefiBootInfo, build_iso};
use std::{
    env, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use env_logger;

mod fastboot;

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

    /// Patch a Bramble Android v3 boot.img template with the generated Image.lz4.
    /// For `run`/`debug`, the patched image is also sent with Fastboot.
    #[arg(long, value_name = "BOOT_IMG")]
    boot_template: Option<PathBuf>,

    /// Output path for --boot-template (defaults beside the template).
    #[arg(long, value_name = "BOOT_IMG")]
    boot_output: Option<PathBuf>,

    /// Put the uncompressed ARM64 Image in the Bramble boot template.
    /// Useful for isolating bootloader LZ4 decompression from kernel entry.
    #[arg(long)]
    boot_uncompressed: bool,

    /// Build the dependency-free AArch64 entry probe. If it reaches Rust,
    /// it resets through PSCI; on Bramble this should return to fastboot.
    #[arg(long)]
    entry_probe: bool,

    /// Build the compressed AArch64 entry probe and halt after Rust entry.
    /// This makes entry success observable instead of resetting into Android.
    #[arg(long)]
    entry_halt_probe: bool,

    /// Build the dependency-free Bramble USB gadget probe.
    #[arg(long)]
    usb_probe: bool,

    /// Build the Bramble USB2 physical pull-up probe without DMA or EP0.
    #[arg(long)]
    usb_pullup_probe: bool,

    /// Build the USB2 pull-up probe and halt instead of resetting on failure.
    #[arg(long)]
    usb_halt_probe: bool,

    /// Build the cold USB3/QMP probe and halt instead of resetting on failure.
    #[arg(long)]
    usb_cold_halt_probe: bool,

    /// Build the minimal Bramble USB2 pull-up probe without UART, reset,
    /// readback, DMA, or EP0 setup.
    #[arg(long)]
    usb_bare_pullup_probe: bool,

    /// Build the Bramble USB2 gadget handoff probe with EP0 descriptors.
    #[arg(long)]
    usb_gadget_handoff_probe: bool,

    /// Run the normal non-destructive USB2 handoff first inside the gadget
    /// probe, retaining the probe's automatic recovery if it fails.
    #[arg(long)]
    usb_gadget_handoff_direct: bool,

    /// Encode EP0/event/TRB observables by dropping the pull-up at a delay
    /// after attach. The host dmesg delta is the diagnostic readout.
    #[arg(long)]
    usb_ep0_signal_probe: bool,

    /// Include a read-only Apps-SMMU SMR/S2CR stream probe in the EP0 signal
    /// probe. The SMMU state takes priority over the runtime signal codes.
    #[arg(long)]
    usb_ep0_signal_smmu_state: bool,

    /// Switch the EP0 signal probe to the USB2 link-state ladder
    /// (DSTS.USBLNKST/RunStop observables) instead of the EP0 event ladder.
    #[arg(long)]
    usb_ep0_signal_link_state: bool,

    /// Encode the raw DSTS.USBLNKST nibble at 2-second resolution instead of
    /// the interpreted link-state ladder.
    #[arg(long)]
    usb_ep0_signal_raw_link: bool,

    /// Drop the pull-up permanently inside the handoff when the selected
    /// condition (1/2/3/5, or 9=unconditional) is observed; the host never
    /// sees the descriptor timeout, so its absence is the readout.
    #[arg(long = "usb-ep0-signal-early-drop", value_name = "CODE")]
    usb_ep0_signal_early_drop: Option<u32>,

    /// Drop the session overrides immediately before the first Run/Stop
    /// (unconditional control for the pull-up ownership question).
    #[arg(long)]
    usb_ep0_signal_pre_drop: bool,

    /// Toggle DCTL Run/Stop in one-second intervals right after the connect.
    #[arg(long)]
    usb_ep0_signal_heartbeat: bool,

    /// Walk the bootloader's Apps-SMMU page tables read-only and relocate the
    /// EP0 DMA objects into the page its live TRANSLATE context already maps.
    #[arg(long)]
    usb_ep0_dma_adopt: bool,

    /// Publish the pull-up only when the Apps-SMMU stream's S2CR type equals
    /// this value (0=fault, 1=bypass, 2=translate; 251..=254 select the
    /// no-match ladder: none-valid, valid-but-unmatched, zero SMRs,
    /// unreadable IDs). The attach itself is the one-bit readout.
    #[arg(long = "usb-ep0-smmu-gate", value_name = "TYPE")]
    usb_ep0_smmu_gate: Option<u32>,

    /// In the EP0 signal probe, drop the pull-up by clearing the QUSB2
    /// VBUSVLDEXT0 session bits as well as the QSCRATCH overrides.
    #[arg(long)]
    usb_ep0_signal_drop_vbus: bool,

    /// Claim a free Apps-SMMU SMR for the DWC3 stream with an S2CR BYPASS
    /// type (readback-verified) before Run/Stop. The pull-up is gated on the
    /// install being accepted.
    #[arg(long)]
    usb_ep0_smmu_install: bool,

    /// Probe event-DMA liveness pre-connect with a CMDIOC command and gate
    /// the pull-up on GEVNTCOUNT actually incrementing.
    #[arg(long)]
    usb_signal_dma_probe: bool,

    /// Make the installed SMR a catch-all (mask all IDs) instead of exact
    /// 0xe0, so a misreported stream ID cannot keep transactions faulting.
    #[arg(long)]
    usb_smmu_install_all: bool,

    /// FSR gate for the DMA probe: 1 = attach only when the Apps-SMMU
    /// recorded a fault during the probe, 2 = attach only when it did not.
    #[arg(long = "usb-signal-fsr-gate", value_name = "MODE")]
    usb_signal_fsr_gate: Option<u32>,

    /// Gate the attach on a CPU readback of the .usb_dma region succeeding.
    #[arg(long)]
    usb_signal_ram_gate: bool,

    /// In signal mode, publish the pull-up even when the handoff failed, so
    /// the command gates stay readable for pre-Run/Stop failures.
    #[arg(long)]
    usb_signal_diag_publish: bool,

    /// Stop all controller MMIO access N seconds after the first Run/Stop
    /// (reboot-cause bisect: external clock collapse vs our own polling).
    #[arg(long = "usb-quiet-after", value_name = "SECS")]
    usb_quiet_after: Option<u64>,

    /// Relocate the linker .usb_dma section to this hex address for the run.
    #[arg(long = "usb-dma-origin", value_name = "ADDR")]
    usb_dma_origin: Option<String>,

    /// Gate the attach on the previous attempt's STARTTRANSFER outcome
    /// (timeout | done | none | hex raw DEPCMD value).
    #[arg(long = "usb-signal-cmd-gate", value_name = "WHEN")]
    usb_signal_cmd_gate: Option<String>,

    /// Gate the attach on the previous attempt's SETTRANSFRESOURCE raw
    /// DEPCMD register (hex; healthy allocation returns 0x10000).
    #[arg(long = "usb-signal-rsc-gate", value_name = "RAW")]
    usb_signal_rsc_gate: Option<String>,

    /// Gate the attach on the previous attempt's DEPSTARTCFG raw DEPCMD
    /// register (hex).
    #[arg(long = "usb-signal-cfg-gate", value_name = "RAW")]
    usb_signal_cfg_gate: Option<String>,

    /// Gate the attach on the captured GCTL.RAMCLKSEL value (0..=3).
    #[arg(long = "usb-signal-ramclk-gate", value_name = "VALUE")]
    usb_signal_ramclk_gate: Option<u32>,

    /// Clear sCR0.SMMUEN/WACFG (readback-verified) before any DWC3 DMA so
    /// unattributed transactions stop stalling in the Apps-SMMU.
    #[arg(long)]
    usb_smmu_disable: bool,

    /// Gate the attach on the DMA-probe event word actually landing in DRAM
    /// (1 = landed, 2 = stayed zero).
    #[arg(long = "usb-signal-evt-data-gate", value_name = "MODE")]
    usb_signal_evt_data_gate: Option<u32>,

    /// Delay only the first handoff attempt's Run/Stop by this many seconds;
    /// the host attach timestamp then identifies the pull-up owner.
    #[arg(long = "usb-connect-delay", value_name = "SECS")]
    usb_connect_delay: Option<u64>,

    /// Build the Bramble SuperSpeed gadget handoff probe with EP0 descriptors.
    #[arg(long)]
    usb_gadget_handoff_super_speed_probe: bool,

    /// Build the gadget handoff probe without reading or changing the Apps
    /// SMMU. This is a hardware differential for a Fastboot-owned bypass.
    #[arg(long)]
    usb_gadget_handoff_no_smmu: bool,

    /// Reuse the Fastboot DWC3 event-ring DMA page for the EP0 probe. This is
    /// a hardware differential for firmware-owned SMMU/DMA visibility.
    #[arg(long)]
    usb_gadget_handoff_reuse_fastboot_dma: bool,

    /// Build the gadget handoff probe without SETTRANSFRESOURCE. This is a
    /// hardware differential for the DWC3 endpoint-resource command.
    #[arg(long)]
    usb_gadget_handoff_no_transfer_resource: bool,

    /// Use Android msm's resource-before-SETEPCONFIG DWC3 ordering.
    #[arg(long)]
    usb_gadget_handoff_android_resource_order: bool,

    /// Bramble differential: connect the DWC3 device before arming EP0
    /// STARTTRANSFER, then arm the SETUP TRB immediately after Run/Stop.
    #[arg(long)]
    usb_gadget_handoff_start_after_connect: bool,

    /// Bramble differential: wait for the host USB Reset event before
    /// arming the initial EP0 SETUP transfer.
    #[arg(long)]
    usb_gadget_handoff_start_after_reset: bool,

    /// Bramble differential: arm the initial EP0 SETUP from Connect Done.
    #[arg(long)]
    usb_gadget_handoff_start_at_connect_done: bool,

    /// Stop the USB2 gadget handoff after a numbered boundary and publish a
    /// temporary physical pull-up for host-side stage diagnostics.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=12))]
    stop_after_stage: Option<u32>,

    /// Run the shared USB EP0 protocol self-test on QEMU virt and exit via
    /// semihosting when it completes.
    #[arg(long)]
    qemu_usb_sim: bool,

    /// Run the QEMU virt USB protocol self-test before building or booting a
    /// Bramble artifact. This validates the shared Rust/DWC3 protocol path;
    /// Qualcomm PHY, Type-C, and SMMU behavior still requires hardware.
    #[arg(long)]
    qemu_preflight: bool,

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

    /// Android boot image for the non-destructive Fastboot `boot` action.
    #[arg(value_name = "IMAGE")]
    image: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Action {
    Build,
    Run,
    Debug,
    Device,
    Boot,
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
            Self::Aarch64 => "fullerene-kernel-aarch64",
        }
    }

    fn cargo_package(self) -> &'static str {
        match self {
            Self::X86_64 => "fullerene-kernel",
            Self::Aarch64 => "fullerene-kernel",
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
            Self::QemuVirt => "virt,gic-version=3",
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

    fn validate_pair(self, arch: Arch) -> io::Result<()> {
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
        Ok(())
    }

    fn validate(self, arch: Arch, action: Action) -> io::Result<()> {
        self.validate_pair(arch)?;
        if action == Action::Boot && (arch != Arch::Aarch64 || self != Self::Bramble) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "boot currently requires the AArch64 bramble platform",
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
        platform.validate(args.arch, args.command)?;
        Ok(Self {
            arch: args.arch,
            platform,
        })
    }
}

#[derive(Clone, Copy)]
struct Aarch64Probe {
    selected: bool,
    flag: &'static str,
    artifact: &'static str,
    env: Option<&'static str>,
    bramble_only: bool,
}

fn aarch64_probe_specs(args: &Args) -> [Aarch64Probe; 9] {
    [
        Aarch64Probe {
            selected: args.entry_probe,
            flag: "--entry-probe",
            artifact: "fullerene-kernel-aarch64-probe",
            env: None,
            bramble_only: false,
        },
        Aarch64Probe {
            selected: args.entry_halt_probe,
            flag: "--entry-halt-probe",
            artifact: "fullerene-kernel-aarch64-entry-halt-probe",
            env: Some("FULLERENE_AARCH64_ENTRY_HALT_PROBE"),
            bramble_only: true,
        },
        Aarch64Probe {
            selected: args.usb_probe,
            flag: "--usb-probe",
            artifact: "fullerene-kernel-aarch64-usb-probe",
            env: None,
            bramble_only: true,
        },
        Aarch64Probe {
            selected: args.usb_pullup_probe,
            flag: "--usb-pullup-probe",
            artifact: "fullerene-kernel-aarch64-usb-probe",
            env: Some("FULLERENE_AARCH64_USB_PULLUP_PROBE"),
            bramble_only: true,
        },
        Aarch64Probe {
            selected: args.usb_halt_probe,
            flag: "--usb-halt-probe",
            artifact: "fullerene-kernel-aarch64-usb-probe",
            env: Some("FULLERENE_AARCH64_USB_HALT_PROBE"),
            bramble_only: true,
        },
        Aarch64Probe {
            selected: args.usb_cold_halt_probe,
            flag: "--usb-cold-halt-probe",
            artifact: "fullerene-kernel-aarch64-usb-probe",
            env: Some("FULLERENE_AARCH64_USB_COLD_HALT_PROBE"),
            bramble_only: true,
        },
        Aarch64Probe {
            selected: args.usb_bare_pullup_probe,
            flag: "--usb-bare-pullup-probe",
            artifact: "fullerene-kernel-aarch64-usb-probe",
            env: Some("FULLERENE_AARCH64_USB_BARE_PULLUP_PROBE"),
            bramble_only: true,
        },
        Aarch64Probe {
            selected: args.usb_gadget_handoff_probe,
            flag: "--usb-gadget-handoff-probe",
            artifact: "fullerene-kernel-aarch64-usb-probe",
            env: Some("FULLERENE_AARCH64_USB_GADGET_HANDOFF_PROBE"),
            bramble_only: true,
        },
        Aarch64Probe {
            selected: args.usb_gadget_handoff_super_speed_probe,
            flag: "--usb-gadget-handoff-super-speed-probe",
            artifact: "fullerene-kernel-aarch64-usb-probe",
            env: Some("FULLERENE_AARCH64_USB_GADGET_HANDOFF_SUPER_SPEED"),
            bramble_only: true,
        },
    ]
}

fn selected_aarch64_probe(args: &Args, target: Target) -> io::Result<Option<Aarch64Probe>> {
    let specs = aarch64_probe_specs(args);
    let selected: Vec<_> = specs
        .iter()
        .copied()
        .filter(|probe| probe.selected)
        .collect();
    if selected.len() > 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "AArch64 probe modes are mutually exclusive",
        ));
    }
    let Some(probe) = selected.first().copied() else {
        return Ok(None);
    };
    if target.arch != Arch::Aarch64
        || target.platform == Platform::PcUefi
        || probe.bramble_only && target.platform != Platform::Bramble
        || args.command != Action::Build
    {
        let platform = if probe.bramble_only {
            "bramble"
        } else {
            "qemu-virt or bramble"
        };
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} requires build --arch aarch64 --platform {}",
                probe.flag, platform
            ),
        ));
    }
    Ok(Some(probe))
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
    if args.command == Action::Device {
        if args.image.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "device does not accept an image path",
            ));
        }
        return fastboot::run_device();
    }
    if args.command == Action::Boot {
        if args.image.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "boot requires an Android boot image path",
            ));
        }
    } else if args.image.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "an image path is only valid with the boot action",
        ));
    }
    let target = Target::from_args(&args)?;
    let profile = BuildProfile::from_debug(args.debug || args.command == Action::Debug);
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("Failed to get workspace root")
        .to_path_buf();

    if args.boot_template.is_some()
        && (target.arch != Arch::Aarch64 || target.platform != Platform::Bramble)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--boot-template is only available for the AArch64 bramble platform",
        ));
    }
    if args.boot_template.is_none() && args.boot_output.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--boot-output requires --boot-template",
        ));
    }
    if args.boot_template.is_none() && args.boot_uncompressed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--boot-uncompressed requires --boot-template",
        ));
    }
    if args.boot_template.is_some()
        && !matches!(args.command, Action::Build | Action::Run | Action::Debug)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--boot-template is only available with build, run, or debug",
        ));
    }
    if target.platform == Platform::Bramble
        && matches!(args.command, Action::Run | Action::Debug)
        && args.boot_template.is_none()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Bramble run/debug requires --boot-template pointing to a stock Android boot.img",
        ));
    }
    if args.qemu_usb_sim
        && (target.arch != Arch::Aarch64
            || target.platform != Platform::QemuVirt
            || !matches!(args.command, Action::Run | Action::Debug))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--qemu-usb-sim requires AArch64 QEMU virt run/debug",
        ));
    }
    if args.qemu_preflight
        && (target.arch != Arch::Aarch64
            || target.platform != Platform::Bramble
            || !matches!(args.command, Action::Build | Action::Run | Action::Debug))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--qemu-preflight requires AArch64 Bramble build/run/debug",
        ));
    }
    if args.usb_gadget_handoff_no_smmu
        && (!args.usb_gadget_handoff_probe && !args.usb_gadget_handoff_super_speed_probe
            || target.arch != Arch::Aarch64
            || target.platform != Platform::Bramble
            || !matches!(args.command, Action::Build | Action::Run | Action::Debug))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb-gadget-handoff-no-smmu requires a Bramble gadget handoff probe on AArch64 build/run/debug",
        ));
    }
    if args.usb_gadget_handoff_direct
        && (!args.usb_gadget_handoff_probe
            || target.arch != Arch::Aarch64
            || target.platform != Platform::Bramble
            || !matches!(args.command, Action::Build | Action::Run | Action::Debug))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb-gadget-handoff-direct requires the Bramble USB2 gadget handoff probe on AArch64 build/run/debug",
        ));
    }
    if args.usb_gadget_handoff_reuse_fastboot_dma
        && (!args.usb_gadget_handoff_probe
            || !args.usb_gadget_handoff_no_smmu
            || target.arch != Arch::Aarch64
            || target.platform != Platform::Bramble
            || !matches!(args.command, Action::Build | Action::Run | Action::Debug))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb-gadget-handoff-reuse-fastboot-dma requires the Bramble USB2 gadget handoff probe with --usb-gadget-handoff-no-smmu on AArch64 build/run/debug",
        ));
    }
    if args.usb_gadget_handoff_no_transfer_resource
        && (!args.usb_gadget_handoff_probe
            || target.arch != Arch::Aarch64
            || target.platform != Platform::Bramble
            || !matches!(args.command, Action::Build | Action::Run | Action::Debug))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb-gadget-handoff-no-transfer-resource requires the Bramble USB2 gadget handoff probe on AArch64 build/run/debug",
        ));
    }
    if args.usb_gadget_handoff_android_resource_order
        && (!args.usb_gadget_handoff_probe
            || target.arch != Arch::Aarch64
            || target.platform != Platform::Bramble
            || !matches!(args.command, Action::Build | Action::Run | Action::Debug))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb-gadget-handoff-android-resource-order requires the Bramble USB2 gadget handoff probe on AArch64 build/run/debug",
        ));
    }
    if args.usb_gadget_handoff_start_after_connect
        && (!args.usb_gadget_handoff_probe
            || !args.usb_gadget_handoff_direct
            || target.arch != Arch::Aarch64
            || target.platform != Platform::Bramble
            || !matches!(args.command, Action::Build | Action::Run | Action::Debug))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb-gadget-handoff-start-after-connect requires the direct Bramble USB2 gadget handoff probe on AArch64 build/run/debug",
        ));
    }
    if args.usb_gadget_handoff_start_after_reset
        && (!args.usb_gadget_handoff_probe
            || !args.usb_gadget_handoff_direct
            || target.arch != Arch::Aarch64
            || target.platform != Platform::Bramble
            || matches!(args.command, Action::Build | Action::Run | Action::Debug) == false)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb-gadget-handoff-start-after-reset requires the direct Bramble USB2 gadget handoff probe on AArch64 build/run/debug",
        ));
    }
    if args.usb_gadget_handoff_start_at_connect_done
        && (!args.usb_gadget_handoff_probe
            || !args.usb_gadget_handoff_direct
            || target.arch != Arch::Aarch64
            || target.platform != Platform::Bramble
            || !matches!(args.command, Action::Build | Action::Run | Action::Debug))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb-gadget-handoff-start-at-connect-done requires the direct Bramble USB2 gadget handoff probe on AArch64 build/run/debug",
        ));
    }
    if (args.usb_gadget_handoff_start_after_connect as u8
        + args.usb_gadget_handoff_start_after_reset as u8
        + args.usb_gadget_handoff_start_at_connect_done as u8)
        > 1
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the Bramble EP0 timing differentials are mutually exclusive",
        ));
    }
    if args.usb_ep0_signal_probe
        && (!args.usb_gadget_handoff_probe
            || !args.usb_gadget_handoff_direct
            || target.arch != Arch::Aarch64
            || target.platform != Platform::Bramble
            || !matches!(args.command, Action::Build | Action::Run | Action::Debug))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb-ep0-signal-probe requires the direct Bramble USB2 gadget handoff probe on AArch64 build/run/debug",
        ));
    }
    if args.usb_ep0_signal_smmu_state && !args.usb_ep0_signal_probe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb-ep0-signal-smmu-state requires --usb-ep0-signal-probe",
        ));
    }
    if args.usb_ep0_signal_link_state && !args.usb_ep0_signal_probe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb-ep0-signal-link-state requires --usb-ep0-signal-probe",
        ));
    }
    if args.usb_ep0_signal_raw_link && !args.usb_ep0_signal_probe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb-ep0-signal-raw-link requires --usb-ep0-signal-probe",
        ));
    }
    if args.stop_after_stage.is_some()
        && (!args.usb_gadget_handoff_probe
            || target.arch != Arch::Aarch64
            || target.platform != Platform::Bramble
            || !matches!(args.command, Action::Build | Action::Run | Action::Debug))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--stop-after-stage requires the Bramble USB2 gadget handoff probe on AArch64 build/run/debug",
        ));
    }
    let selected_probe = selected_aarch64_probe(&args, target)?;

    if target.arch == Arch::Aarch64 {
        if args.clone_ovmf || args.iso_only {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "OVMF and ISO options are only available for the x86_64 pc-uefi platform",
            ));
        }

        if args.command == Action::Boot {
            if target.platform != Platform::Bramble {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "boot currently requires --platform bramble",
                ));
            }
            let image = args.image.as_deref().unwrap();
            audit_android_boot_image(image)?;
            return fastboot::run_boot(image);
        }

        if args.qemu_preflight {
            run_aarch64_qemu_preflight(&workspace_root, profile, args.timeout.or(Some(10)))?;
        }

        let kernel_artifact = selected_probe
            .map(|probe| probe.artifact)
            .unwrap_or_else(|| target.arch.kernel_artifact());
        let kernel_path = build_aarch64_kernel(
            &workspace_root,
            profile,
            target.platform,
            kernel_artifact,
            selected_probe.and_then(|probe| probe.env),
            args.usb_gadget_handoff_no_smmu,
            args.usb_gadget_handoff_reuse_fastboot_dma,
            args.usb_gadget_handoff_no_transfer_resource,
            args.usb_gadget_handoff_android_resource_order,
            args.usb_gadget_handoff_start_after_connect,
            args.usb_gadget_handoff_start_after_reset,
            args.usb_gadget_handoff_start_at_connect_done,
            args.stop_after_stage,
            args.qemu_usb_sim,
            args.usb_gadget_handoff_direct,
            args.usb_ep0_signal_probe,
            args.usb_ep0_signal_smmu_state,
            args.usb_ep0_signal_link_state,
            args.usb_ep0_signal_raw_link,
            args.usb_ep0_signal_early_drop,
            args.usb_ep0_signal_pre_drop,
            args.usb_ep0_signal_heartbeat,
            args.usb_ep0_dma_adopt,
            args.usb_ep0_smmu_gate,
            args.usb_ep0_signal_drop_vbus,
            args.usb_connect_delay,
            args.usb_ep0_smmu_install,
            args.usb_signal_dma_probe,
            args.usb_smmu_install_all,
            args.usb_signal_fsr_gate,
            args.usb_signal_ram_gate,
            args.usb_signal_diag_publish,
            args.usb_quiet_after,
            args.usb_dma_origin,
            args.usb_signal_cmd_gate,
            args.usb_signal_rsc_gate,
            args.usb_signal_cfg_gate,
            args.usb_signal_ramclk_gate,
            args.usb_smmu_disable,
            args.usb_signal_evt_data_gate,
        )?;
        if target.platform == Platform::Bramble
            && matches!(args.command, Action::Run | Action::Debug)
        {
            let raw_kernel_path = build_aarch64_raw_kernel(&kernel_path)?;
            let image_path = build_aarch64_image(&raw_kernel_path)?;
            let image_lz4_path = build_aarch64_lz4(&image_path)?;
            let template = args.boot_template.as_deref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Bramble run/debug requires --boot-template",
                )
            })?;
            let output = args.boot_output.clone().unwrap_or_else(|| {
                env::temp_dir().join(format!("fullerene-bramble-boot-{}.img", std::process::id()))
            });
            let boot_kernel = if args.boot_uncompressed {
                &image_path
            } else {
                &image_lz4_path
            };
            patch_bramble_boot_image(template, boot_kernel, &output)?;
            audit_bramble_boot_image(template, boot_kernel, &output)?;
            println!(
                "Bramble boot image prepared at {}; sending with Fastboot",
                output.display()
            );
            return fastboot::run_boot(&output);
        }
        if args.command == Action::Build {
            let raw_kernel_path = build_aarch64_raw_kernel(&kernel_path)?;
            let image_path = build_aarch64_image(&raw_kernel_path)?;
            let image_lz4_path = build_aarch64_lz4(&image_path)?;
            if let Some(probe) = selected_probe {
                println!(
                    "AArch64 {} built at {}",
                    probe.flag.trim_start_matches("--"),
                    kernel_path.display()
                );
            } else {
                println!("AArch64 ELF kernel built at {}", kernel_path.display());
            }
            println!("AArch64 raw kernel built at {}", raw_kernel_path.display());
            println!("AArch64 Image built at {}", image_path.display());
            println!("AArch64 Image.lz4 built at {}", image_lz4_path.display());
            if let Some(template) = args.boot_template.as_deref() {
                let output = args.boot_output.clone().unwrap_or_else(|| {
                    template
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join("fullerene-bramble-boot.img")
                });
                let boot_kernel = if args.boot_uncompressed {
                    &image_path
                } else {
                    &image_lz4_path
                };
                patch_bramble_boot_image(template, boot_kernel, &output)?;
                audit_bramble_boot_image(template, boot_kernel, &output)?;
                println!(
                    "Bramble temporary boot image built at {} (use only with an unlocked device)",
                    output.display()
                );
            }
            return Ok(());
        }

        let qemu_artifact = if target.platform == Platform::QemuVirt {
            // QEMU passes the explicitly supplied DTB in x0 for an arm64
            // Linux Image. It does not do that reliably for a freestanding
            // ELF, so keep the ELF as the build artifact but boot the same
            // Image format that the Android loader will consume.
            let raw_kernel_path = build_aarch64_raw_kernel(&kernel_path)?;
            build_aarch64_image(&raw_kernel_path)?
        } else {
            kernel_path
        };
        run_aarch64_qemu(
            &qemu_artifact,
            target.platform,
            args.command == Action::Debug,
            args.timeout.or_else(|| args.qemu_usb_sim.then_some(10)),
            args.qemu_usb_sim,
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

fn build_aarch64_kernel(
    workspace_root: &Path,
    profile: BuildProfile,
    platform: Platform,
    kernel_artifact: &str,
    probe_env: Option<&str>,
    gadget_handoff_no_smmu: bool,
    gadget_handoff_reuse_fastboot_dma: bool,
    gadget_handoff_no_transfer_resource: bool,
    gadget_handoff_android_resource_order: bool,
    gadget_handoff_start_after_connect: bool,
    gadget_handoff_start_after_reset: bool,
    gadget_handoff_start_at_connect_done: bool,
    gadget_handoff_stop_after_stage: Option<u32>,
    qemu_usb_sim: bool,
    gadget_handoff_direct: bool,
    ep0_signal_probe: bool,
    ep0_signal_smmu_state: bool,
    ep0_signal_link_state: bool,
    ep0_signal_raw_link: bool,
    ep0_signal_early_drop: Option<u32>,
    ep0_signal_pre_drop: bool,
    ep0_signal_heartbeat: bool,
    ep0_dma_adopt: bool,
    ep0_smmu_gate: Option<u32>,
    ep0_signal_drop_vbus: bool,
    connect_delay: Option<u64>,
    ep0_smmu_install: bool,
    signal_dma_probe: bool,
    smmu_install_all: bool,
    signal_fsr_gate: Option<u32>,
    signal_ram_gate: bool,
    signal_diag_publish: bool,
    quiet_after: Option<u64>,
    dma_origin: Option<String>,
    signal_cmd_gate: Option<String>,
    signal_rsc_gate: Option<String>,
    signal_cfg_gate: Option<String>,
    signal_ramclk_gate: Option<u32>,
    smmu_disable: bool,
    signal_evt_data_gate: Option<u32>,
) -> io::Result<PathBuf> {
    let target = Arch::Aarch64;
    let mut cargo = Command::new("cargo");
    cargo
        .current_dir(workspace_root)
        .args([
            "build",
            "-q",
            "--package",
            target.cargo_package(),
            "--features",
            "aarch64",
            "--bin",
            kernel_artifact,
            "--target",
            target.rust_target(),
            "--profile",
            profile.cargo_name(),
        ])
        // Keep the linker choice in the architecture-specific build path. The
        // bare-metal target is shipped with Rust's lld linker, so this does not
        // require a host C cross-toolchain.
        .env("CARGO_TARGET_AARCH64_UNKNOWN_NONE_LINKER", "rust-lld")
        .env(
            "FULLERENE_AARCH64_PLATFORM",
            match platform {
                Platform::Bramble => "bramble",
                Platform::QemuVirt => "qemu-virt",
                Platform::PcUefi => "pc-uefi",
            },
        );
    if let Some(probe_env) = probe_env {
        cargo.env(probe_env, "1");
    }
    if gadget_handoff_no_smmu {
        cargo.env("FULLERENE_AARCH64_USB_GADGET_HANDOFF_NO_SMMU", "1");
    }
    if gadget_handoff_reuse_fastboot_dma {
        cargo.env(
            "FULLERENE_AARCH64_USB_GADGET_HANDOFF_REUSE_FASTBOOT_DMA",
            "1",
        );
    }
    if gadget_handoff_no_transfer_resource {
        cargo.env(
            "FULLERENE_AARCH64_USB_GADGET_HANDOFF_NO_TRANSFER_RESOURCE",
            "1",
        );
    }
    if gadget_handoff_android_resource_order {
        cargo.env(
            "FULLERENE_AARCH64_USB_GADGET_HANDOFF_ANDROID_RESOURCE_ORDER",
            "1",
        );
    }
    if gadget_handoff_start_after_connect {
        cargo.env(
            "FULLERENE_AARCH64_USB_GADGET_HANDOFF_START_AFTER_CONNECT",
            "1",
        );
    }
    if gadget_handoff_start_after_reset {
        cargo.env(
            "FULLERENE_AARCH64_USB_GADGET_HANDOFF_START_AFTER_RESET",
            "1",
        );
    }
    if gadget_handoff_start_at_connect_done {
        cargo.env(
            "FULLERENE_AARCH64_USB_GADGET_HANDOFF_START_AT_CONNECT_DONE",
            "1",
        );
    }
    if let Some(stage) = gadget_handoff_stop_after_stage {
        cargo.env(
            "FULLERENE_AARCH64_USB_GADGET_HANDOFF_STOP_STAGE",
            stage.to_string(),
        );
    }
    if qemu_usb_sim {
        cargo.env("FULLERENE_AARCH64_QEMU_USB_SIM", "1");
    }
    if gadget_handoff_direct {
        cargo.env("FULLERENE_AARCH64_USB_GADGET_HANDOFF_DIRECT", "1");
    }
    if ep0_signal_probe {
        cargo.env("FULLERENE_AARCH64_USB_EP0_SIGNAL_PROBE", "1");
    }
    if ep0_signal_smmu_state {
        cargo.env("FULLERENE_AARCH64_USB_EP0_SIGNAL_SMMU_STATE", "1");
    }
    if ep0_signal_link_state {
        cargo.env("FULLERENE_AARCH64_USB_EP0_SIGNAL_LINK_STATE", "1");
    }
    if ep0_signal_raw_link {
        cargo.env("FULLERENE_AARCH64_USB_EP0_SIGNAL_RAW_LINK", "1");
    }
    if let Some(code) = ep0_signal_early_drop {
        cargo.env(
            "FULLERENE_AARCH64_USB_EP0_SIGNAL_EARLY_DROP",
            code.to_string(),
        );
    }
    if ep0_signal_pre_drop {
        cargo.env("FULLERENE_AARCH64_USB_EP0_SIGNAL_PRE_DROP", "1");
    }
    if ep0_signal_heartbeat {
        cargo.env("FULLERENE_AARCH64_USB_EP0_SIGNAL_HEARTBEAT", "1");
    }
    if ep0_dma_adopt {
        cargo.env("FULLERENE_AARCH64_USB_EP0_DMA_ADOPT", "1");
    }
    if let Some(value) = ep0_smmu_gate {
        cargo.env("FULLERENE_AARCH64_USB_EP0_SMMU_GATE", value.to_string());
    }
    if ep0_signal_drop_vbus {
        cargo.env("FULLERENE_AARCH64_USB_SIGNAL_DROP_VBUS", "1");
    }
    if let Some(secs) = connect_delay {
        cargo.env("FULLERENE_AARCH64_USB_CONNECT_DELAY", secs.to_string());
    }
    if ep0_smmu_install {
        cargo.env("FULLERENE_AARCH64_USB_EP0_SMMU_INSTALL", "1");
    }
    if signal_dma_probe {
        cargo.env("FULLERENE_AARCH64_USB_SIGNAL_DMA_PROBE", "1");
    }
    if smmu_install_all {
        cargo.env("FULLERENE_AARCH64_USB_SMMU_INSTALL_ALL", "1");
    }
    if let Some(mode) = signal_fsr_gate {
        cargo.env("FULLERENE_AARCH64_USB_SIGNAL_FSR_GATE", mode.to_string());
    }
    if signal_ram_gate {
        cargo.env("FULLERENE_AARCH64_USB_SIGNAL_RAM_GATE", "1");
    }
    if signal_diag_publish {
        cargo.env("FULLERENE_AARCH64_USB_SIGNAL_DIAG_PUBLISH", "1");
    }
    if let Some(secs) = quiet_after {
        cargo.env("FULLERENE_AARCH64_USB_QUIET_AFTER", secs.to_string());
    }
    if let Some(origin) = dma_origin {
        cargo.env("FULLERENE_AARCH64_USB_DMA_ORIGIN", origin);
    }
    if let Some(value) = signal_cmd_gate {
        cargo.env("FULLERENE_AARCH64_USB_SIGNAL_CMD_GATE", value);
    }
    if let Some(value) = signal_rsc_gate {
        cargo.env("FULLERENE_AARCH64_USB_SIGNAL_RSC_GATE", value);
    }
    if let Some(value) = signal_cfg_gate {
        cargo.env("FULLERENE_AARCH64_USB_SIGNAL_CFG_GATE", value);
    }
    if let Some(value) = signal_ramclk_gate {
        cargo.env(
            "FULLERENE_AARCH64_USB_SIGNAL_RAMCLK_GATE",
            value.to_string(),
        );
    }
    if smmu_disable {
        cargo.env("FULLERENE_AARCH64_USB_SMMU_DISABLE", "1");
    }
    if let Some(mode) = signal_evt_data_gate {
        cargo.env(
            "FULLERENE_AARCH64_USB_SIGNAL_EVT_DATA_GATE",
            mode.to_string(),
        );
    }

    // Android's Bramble bootloader may relocate an arm64 Image. Build the
    // freestanding binary as a static PIE and let the Rust bootstrap apply
    // its R_AARCH64_RELATIVE entries before normal Rust code runs.
    let mut rustflags = env::var("RUSTFLAGS").unwrap_or_default();
    rustflags.push_str(" -C relocation-model=pic -C link-arg=-pie");
    rustflags.push_str(" -C link-arg=-z -C link-arg=notext");
    cargo.env("RUSTFLAGS", rustflags);

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
        .join(kernel_artifact);
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

fn run_aarch64_qemu_preflight(
    workspace_root: &Path,
    profile: BuildProfile,
    timeout: Option<u64>,
) -> io::Result<()> {
    println!("QEMU Bramble preflight: building the qemu-virt self-test artifact");
    let kernel = build_aarch64_kernel(
        workspace_root,                  // 1
        profile,                         // 2
        Platform::QemuVirt,              // 3
        Arch::Aarch64.kernel_artifact(), // 4
        None,                            // 5  probe_env
        false,                           // 6  no_smmu
        false,                           // 7  reuse_fastboot_dma
        false,                           // 8  no_transfer_resource
        false,                           // 9  android_resource_order
        false,                           // 10 start_after_connect
        false,                           // 11 start_after_reset
        false,                           // 12 start_at_connect_done
        None,                            // 13 stop_after_stage
        true,                            // 14 qemu_usb_sim
        false,                           // 15 gadget_handoff_direct
        false,                           // 16 ep0_signal_probe
        false,                           // 17 ep0_signal_smmu_state
        false,                           // 18 ep0_signal_link_state
        false,                           // 19 ep0_signal_raw_link
        None,                            // 20 ep0_signal_early_drop
        false,                           // 21 ep0_signal_pre_drop
        false,                           // 22 ep0_signal_heartbeat
        false,                           // 23 ep0_dma_adopt
        None,                            // 24 ep0_smmu_gate
        false,                           // 25 ep0_signal_drop_vbus
        None,                            // 26 connect_delay
        false,                           // 27 ep0_smmu_install
        false,                           // 28 signal_dma_probe
        false,                           // 29 smmu_install_all
        None,                            // 30 signal_fsr_gate
        false,                           // 31 signal_ram_gate
        false,                           // 32 signal_diag_publish
        None,                            // 33 quiet_after
        None,                            // 34 dma_origin
        None,                            // 35 signal_cmd_gate
        None,                            // 36 signal_rsc_gate
        None,                            // 37 signal_cfg_gate
        None,                            // 38 signal_ramclk_gate
        false,                           // 39 smmu_disable
        None,                            // 40 signal_evt_data_gate
    )?;
    let raw = build_aarch64_raw_kernel(&kernel)?;
    let image = build_aarch64_image(&raw)?;
    run_aarch64_qemu(&image, Platform::QemuVirt, false, timeout, true)?;
    println!("QEMU Bramble preflight: PASS");
    Ok(())
}

fn build_aarch64_raw_kernel(elf: &Path) -> io::Result<PathBuf> {
    let raw = elf.with_extension("bin");
    let mut failures = Vec::new();

    for objcopy in aarch64_objcopy_candidates() {
        let version = Command::new(&objcopy)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !matches!(version, Ok(status) if status.success()) {
            continue;
        }

        let status = Command::new(&objcopy)
            .args(["-O", "binary"])
            .arg(elf)
            .arg(&raw)
            .status();
        match status {
            Ok(status) if status.success() && raw.is_file() => return Ok(raw),
            Ok(status) => failures.push(format!("{} exited with {status}", objcopy.display())),
            Err(error) => failures.push(format!("{}: {error}", objcopy.display())),
        }
        let _ = fs::remove_file(&raw);
    }

    if failures.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no objcopy was found for AArch64 raw output; install llvm-tools-preview or binutils for AArch64",
        ));
    }
    Err(io::Error::other(format!(
        "failed to convert AArch64 ELF kernel {} to raw binary; tried: {}",
        elf.display(),
        failures.join(", ")
    )))
}

fn aarch64_objcopy_candidates() -> Vec<PathBuf> {
    let mut candidates = [
        "llvm-objcopy",
        "rust-objcopy",
        "aarch64-linux-gnu-objcopy",
        "objcopy",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect::<Vec<_>>();

    // `llvm-tools-preview` installs llvm-objcopy inside the Rust sysroot but
    // does not put it on PATH. This is the reliable fallback on CI runners
    // that provide the component without installing a host LLVM package.
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    if let Ok(output) = Command::new(rustc).args(["--print", "sysroot"]).output()
        && output.status.success()
    {
        let rustlib =
            PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()).join("lib/rustlib");
        if let Ok(hosts) = fs::read_dir(rustlib) {
            for host in hosts.flatten() {
                let candidate = host.path().join("bin/llvm-objcopy");
                if candidate.is_file() {
                    candidates.insert(2, candidate);
                }
            }
        }
    }

    candidates
}

/// Wrap the freestanding entry point in the 64-byte arm64 Linux Image header.
///
/// The first instruction branches over the header into Fullerene's entry
/// point. This keeps the payload usable by Android boot image loaders that
/// expect an uncompressed AArch64 Image rather than an ELF or a naked flat
/// binary.
fn build_aarch64_image(raw: &Path) -> io::Result<PathBuf> {
    let payload = fs::read(raw)?;
    let image = make_aarch64_image(&payload);
    let image_path = raw.with_extension("Image");
    fs::write(&image_path, image)?;
    audit_aarch64_image(&image_path, raw)?;
    Ok(image_path)
}

fn make_aarch64_image(payload: &[u8]) -> Vec<u8> {
    const IMAGE_HEADER_SIZE: usize = AARCH64_IMAGE_HEADER_SIZE;
    const TEXT_OFFSET: u64 = AARCH64_IMAGE_TEXT_OFFSET;
    // The freestanding image has a sizeable zero-initialized bootstrap heap
    // and stack which are not present in the flat payload emitted by objcopy.
    // Advertise the mapped image footprint, not only the file length, so an
    // Android bootloader will keep the kernel's .bss out of its workspace.
    // The linker reserves the bootstrap BSS after the 0x80000 text offset.
    // Keep a rounded-up 4 MiB footprint in the arm64 header so Android's
    // bootloader does not reuse the tail of that reservation as workspace.
    const IMAGE_MEMORY_SIZE: u64 = AARCH64_IMAGE_MEMORY_SIZE;
    const FLAG_PAGE_SIZE_4K: u64 = AARCH64_IMAGE_FLAG_PAGE_SIZE_4K;
    const ARM64_IMAGE_MAGIC: u32 = AARCH64_IMAGE_MAGIC;

    let mut image = Vec::with_capacity(IMAGE_HEADER_SIZE + payload.len());
    // b +64: Fullerene's entry point follows the Linux Image metadata.
    image.extend_from_slice(&0x1400_0010u32.to_le_bytes());
    image.extend_from_slice(&0xd503_201fu32.to_le_bytes());
    image.extend_from_slice(&TEXT_OFFSET.to_le_bytes());
    image.extend_from_slice(
        &((IMAGE_HEADER_SIZE + payload.len()) as u64)
            .max(IMAGE_MEMORY_SIZE)
            .to_le_bytes(),
    );
    // Fullerene's freestanding payload is linked at the Bramble DRAM base
    // plus text_offset; unlike a relocatable Linux Image it cannot be placed
    // at an arbitrary physical base.
    image.extend_from_slice(&FLAG_PAGE_SIZE_4K.to_le_bytes());
    image.extend_from_slice(&0u64.to_le_bytes());
    image.extend_from_slice(&0u64.to_le_bytes());
    image.extend_from_slice(&0u64.to_le_bytes());
    image.extend_from_slice(&ARM64_IMAGE_MAGIC.to_le_bytes());
    image.extend_from_slice(&0u32.to_le_bytes());
    debug_assert_eq!(image.len(), IMAGE_HEADER_SIZE);
    image.extend_from_slice(payload);
    image
}

/// Emit the LZ4 frame used by the Bramble Android 14 kernel without requiring
/// a host lz4 executable.
///
/// The stock Bramble `Image.lz4` is a modern LZ4 frame (magic `04 22 4d 18`),
/// not the older legacy stream. Each block below is a valid literal-only LZ4
/// block. It is intentionally simple, but uses a normal compressed block
/// rather than the stored-block extension accepted by newer LZ4 readers; a
/// few Android bootloaders only implement the former.
fn build_aarch64_lz4(image: &Path) -> io::Result<PathBuf> {
    let payload = fs::read(image)?;
    let compressed = make_lz4_frame(&payload);
    let output = image.with_extension("Image.lz4");
    fs::write(&output, compressed)?;
    audit_lz4_frame(&output, image)?;
    Ok(output)
}

const AARCH64_IMAGE_HEADER_SIZE: usize = 64;
const AARCH64_IMAGE_TEXT_OFFSET: u64 = 0x0008_0000;
const AARCH64_IMAGE_MEMORY_SIZE: u64 = 0x0040_0000;
const AARCH64_IMAGE_FLAG_PAGE_SIZE_4K: u64 = 1 << 1;
const AARCH64_IMAGE_MAGIC: u32 = 0x644d_5241;

fn audit_aarch64_image(image: &Path, raw: &Path) -> io::Result<()> {
    let image_bytes = fs::read(image)?;
    let raw_bytes = fs::read(raw)?;
    audit_aarch64_image_bytes(&image_bytes, &raw_bytes).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "AArch64 Image audit failed for {}: {error}",
                image.display()
            ),
        )
    })
}

fn audit_aarch64_image_bytes(image: &[u8], raw: &[u8]) -> io::Result<()> {
    if image.len() != AARCH64_IMAGE_HEADER_SIZE + raw.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Image length is {}, expected {}",
                image.len(),
                AARCH64_IMAGE_HEADER_SIZE + raw.len()
            ),
        ));
    }
    if read_u32(image, 0)? != 0x1400_0010 || read_u32(image, 4)? != 0xd503_201f {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Image entry header is not the Fullerene branch/NOP pair",
        ));
    }
    if read_u64(image, 8)? != AARCH64_IMAGE_TEXT_OFFSET {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Image text offset does not target the Bramble load contract",
        ));
    }
    let advertised_size = read_u64(image, 16)?;
    let expected_size = (image.len() as u64).max(AARCH64_IMAGE_MEMORY_SIZE);
    if advertised_size != expected_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Image advertised memory size {advertised_size:#x}, expected {expected_size:#x}"
            ),
        ));
    }
    if read_u64(image, 24)? != AARCH64_IMAGE_FLAG_PAGE_SIZE_4K
        || read_u32(image, 56)? != AARCH64_IMAGE_MAGIC
        || read_u32(image, 60)? != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Image flags, magic, or reserved field is invalid",
        ));
    }
    if &image[AARCH64_IMAGE_HEADER_SIZE..] != raw {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Image payload differs from the raw AArch64 kernel",
        ));
    }
    Ok(())
}

fn audit_lz4_frame(frame: &Path, image: &Path) -> io::Result<()> {
    let frame_bytes = fs::read(frame)?;
    let image_bytes = fs::read(image)?;
    validate_bramble_lz4_frame(&frame_bytes)?;
    let mut decoder = lz4_flex::frame::FrameDecoder::new(fs::File::open(frame)?);
    let mut decoded = Vec::new();
    decoder.read_to_end(&mut decoded)?;
    if decoded != image_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Image.lz4 audit failed for {}: decoded payload differs from {}",
                frame.display(),
                image.display()
            ),
        ));
    }
    Ok(())
}

/// Check the restrictions imposed by the Bramble boot path independently of
/// the standard LZ4 decoder used by `audit_lz4_frame`.
fn validate_bramble_lz4_frame(frame: &[u8]) -> io::Result<()> {
    const LZ4_FRAME_MAGIC: [u8; 4] = [0x04, 0x22, 0x4d, 0x18];
    const FLG: u8 = 0x64;
    const BD: u8 = 0x70;
    const BLOCK_MAX: usize = 4 * 1024 * 1024;

    if frame.len() < 11 || frame[..4] != LZ4_FRAME_MAGIC || frame[4] != FLG || frame[5] != BD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Bramble requires an independent, checksummed 4 MiB LZ4 frame",
        ));
    }
    if frame[6] != (xxhash32(&frame[4..6], 0) >> 8) as u8 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "LZ4 frame descriptor checksum mismatch",
        ));
    }

    let mut cursor = 7;
    loop {
        let block_size = read_u32(frame, cursor)? as usize;
        cursor = cursor
            .checked_add(4)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "LZ4 cursor overflow"))?;
        if block_size == 0 {
            break;
        }
        if block_size > BLOCK_MAX || block_size > frame.len().saturating_sub(cursor) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LZ4 block exceeds the frame",
            ));
        }
        let block = &frame[cursor..cursor + block_size];
        cursor += block_size;
        let token = *block
            .first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty LZ4 block"))?;
        if token & 0x0f != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Bramble only accepts literal-only LZ4 blocks",
            ));
        }
        let mut block_cursor = 1;
        let mut literal_len = (token >> 4) as usize;
        if literal_len == 15 {
            loop {
                let extension = *block.get(block_cursor).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "truncated LZ4 literal length")
                })?;
                block_cursor += 1;
                literal_len = literal_len.checked_add(extension as usize).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "LZ4 literal length overflow")
                })?;
                if extension != 255 {
                    break;
                }
            }
        }
        if literal_len != block.len().saturating_sub(block_cursor) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LZ4 literal block has an inconsistent length",
            ));
        }
    }
    if cursor.checked_add(4) != Some(frame.len()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trailing bytes after LZ4 content checksum",
        ));
    }
    Ok(())
}

#[cfg(test)]
fn decode_literal_lz4_frame(frame: &[u8]) -> io::Result<Vec<u8>> {
    const LZ4_FRAME_MAGIC: [u8; 4] = [0x04, 0x22, 0x4d, 0x18];
    const FLG: u8 = 0x64;
    const BD: u8 = 0x70;
    const BLOCK_MAX: usize = 4 * 1024 * 1024;

    if frame.len() < 11 || frame[..4] != LZ4_FRAME_MAGIC || frame[4] != FLG || frame[5] != BD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported or truncated LZ4 frame header",
        ));
    }
    if frame[6] != (xxhash32(&frame[4..6], 0) >> 8) as u8 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "LZ4 frame descriptor checksum mismatch",
        ));
    }

    let mut cursor = 7;
    let mut decoded = Vec::new();
    loop {
        let block_size = read_u32(frame, cursor)? as usize;
        cursor = cursor
            .checked_add(4)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "LZ4 cursor overflow"))?;
        if block_size == 0 {
            break;
        }
        if block_size > BLOCK_MAX || block_size > frame.len().saturating_sub(cursor) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LZ4 block exceeds the frame",
            ));
        }
        let block = &frame[cursor..cursor + block_size];
        cursor += block_size;
        let token = *block
            .first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty LZ4 block"))?;
        if token & 0x0f != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "LZ4 audit only accepts Fullerene literal-only blocks",
            ));
        }
        let mut block_cursor = 1;
        let mut literal_len = (token >> 4) as usize;
        if literal_len == 15 {
            loop {
                let extension = *block.get(block_cursor).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "truncated LZ4 literal length")
                })?;
                block_cursor += 1;
                literal_len = literal_len.checked_add(extension as usize).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "LZ4 literal length overflow")
                })?;
                if extension != 255 {
                    break;
                }
            }
        }
        if literal_len != block.len().saturating_sub(block_cursor) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LZ4 literal block has an inconsistent length",
            ));
        }
        decoded.extend_from_slice(&block[block_cursor..]);
    }
    let checksum = read_u32(frame, cursor)?;
    cursor += 4;
    if cursor != frame.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trailing bytes after LZ4 content checksum",
        ));
    }
    if checksum != xxhash32(&decoded, 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "LZ4 content checksum mismatch",
        ));
    }
    Ok(decoded)
}

fn make_lz4_frame(payload: &[u8]) -> Vec<u8> {
    const LZ4_FRAME_MAGIC: u32 = 0x184d_2204;
    const FLG: u8 = 0x64; // version 01, independent blocks, content checksum
    const BD: u8 = 0x70; // 4 MiB maximum block size
    const BLOCK_MAX: usize = 4 * 1024 * 1024;
    // Literal-only encoding adds one token byte and one length byte for each
    // 255 bytes after the first 15. Keep the encoded block within the BD
    // maximum instead of splitting the unencoded payload at that boundary.
    const PAYLOAD_MAX: usize = BLOCK_MAX - (2 + BLOCK_MAX / 255);

    let mut frame = Vec::with_capacity(4 + 3 + payload.len() + payload.len() / BLOCK_MAX * 4 + 8);
    frame.extend_from_slice(&LZ4_FRAME_MAGIC.to_le_bytes());
    frame.extend_from_slice(&[FLG, BD]);
    frame.push((xxhash32(&[FLG, BD], 0) >> 8) as u8);
    for block in payload.chunks(PAYLOAD_MAX) {
        let encoded = encode_lz4_literals(block);
        debug_assert!(encoded.len() <= BLOCK_MAX);
        let block_size = u32::try_from(encoded.len()).expect("LZ4 block size fits in u32");
        frame.extend_from_slice(&block_size.to_le_bytes());
        frame.extend_from_slice(&encoded);
    }
    frame.extend_from_slice(&0u32.to_le_bytes());
    frame.extend_from_slice(&xxhash32(payload, 0).to_le_bytes());
    frame
}

fn encode_lz4_literals(payload: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(payload.len() + payload.len() / 255 + 2);
    let literal_length = payload.len();
    encoded.push((literal_length.min(15) as u8) << 4);
    if literal_length >= 15 {
        let mut remaining = literal_length - 15;
        while remaining >= 255 {
            encoded.push(255);
            remaining -= 255;
        }
        encoded.push(remaining as u8);
    }
    encoded.extend_from_slice(payload);
    encoded
}

fn xxhash32(input: &[u8], seed: u32) -> u32 {
    const PRIME1: u32 = 2_654_435_761;
    const PRIME2: u32 = 2_246_822_519;
    const PRIME3: u32 = 3_266_489_917;
    const PRIME4: u32 = 668_265_263;
    const PRIME5: u32 = 374_761_393;

    let mut index = 0;
    let mut hash;
    if input.len() >= 16 {
        let mut v1 = seed.wrapping_add(PRIME1).wrapping_add(PRIME2);
        let mut v2 = seed.wrapping_add(PRIME2);
        let mut v3 = seed;
        let mut v4 = seed.wrapping_sub(PRIME1);
        while index <= input.len() - 16 {
            v1 = xxhash_round(
                v1,
                u32::from_le_bytes(input[index..index + 4].try_into().unwrap()),
            );
            v2 = xxhash_round(
                v2,
                u32::from_le_bytes(input[index + 4..index + 8].try_into().unwrap()),
            );
            v3 = xxhash_round(
                v3,
                u32::from_le_bytes(input[index + 8..index + 12].try_into().unwrap()),
            );
            v4 = xxhash_round(
                v4,
                u32::from_le_bytes(input[index + 12..index + 16].try_into().unwrap()),
            );
            index += 16;
        }
        hash = v1
            .rotate_left(1)
            .wrapping_add(v2.rotate_left(7))
            .wrapping_add(v3.rotate_left(12))
            .wrapping_add(v4.rotate_left(18));
    } else {
        hash = seed.wrapping_add(PRIME5);
    }
    hash = hash.wrapping_add(input.len() as u32);
    while index + 4 <= input.len() {
        hash = hash.wrapping_add(
            u32::from_le_bytes(input[index..index + 4].try_into().unwrap()).wrapping_mul(PRIME3),
        );
        hash = hash.rotate_left(17).wrapping_mul(PRIME4);
        index += 4;
    }
    while index < input.len() {
        hash = hash.wrapping_add((input[index] as u32).wrapping_mul(PRIME5));
        hash = hash.rotate_left(11).wrapping_mul(PRIME1);
        index += 1;
    }
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(PRIME2);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(PRIME3);
    hash ^ (hash >> 16)
}

fn xxhash_round(accumulator: u32, input: u32) -> u32 {
    accumulator
        .wrapping_add(input.wrapping_mul(2_246_822_519))
        .rotate_left(13)
        .wrapping_mul(2_654_435_761)
}

fn patch_bramble_boot_image(template: &Path, kernel: &Path, output: &Path) -> io::Result<()> {
    const PAGE_SIZE: usize = 4096;
    const HEADER_SIZE_OFFSET: usize = 20;
    const HEADER_VERSION_OFFSET: usize = 40;
    const KERNEL_SIZE_OFFSET: usize = 8;
    const RAMDISK_SIZE_OFFSET: usize = 12;

    let template_bytes = fs::read(template)?;
    if template == output {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "boot template and output path must be different",
        ));
    }
    if template_bytes.len() < PAGE_SIZE || &template_bytes[..8] != b"ANDROID!" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Bramble boot template is not an Android boot image",
        ));
    }
    let header_version = read_le_u32(&template_bytes, HEADER_VERSION_OFFSET)?;
    if header_version != 3 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "Bramble boot patcher supports Android boot header v3, found v{header_version}"
            ),
        ));
    }
    let header_size = read_le_u32(&template_bytes, HEADER_SIZE_OFFSET)? as usize;
    if header_size == 0 || header_size > PAGE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid Android v3 boot header size {header_size}"),
        ));
    }
    // A factory boot partition image commonly ends with an AVB vbmeta block
    // and a 64-byte AVB footer. The temporary `fastboot boot` path is intended
    // for an unlocked device, so do not carry stale hashes over a replacement
    // kernel. Keep only the original Android boot image before the AVB block.
    let template_bytes = strip_avb_metadata(&template_bytes)?;
    if template_bytes.len() < PAGE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "AVB-stripped boot template is smaller than one Android page",
        ));
    }

    let old_kernel_size = read_le_u32(&template_bytes, KERNEL_SIZE_OFFSET)? as usize;
    let ramdisk_size = read_le_u32(&template_bytes, RAMDISK_SIZE_OFFSET)? as usize;
    let old_kernel_offset = align_up_checked(PAGE_SIZE, PAGE_SIZE)?;
    let old_ramdisk_offset = align_up_checked(
        old_kernel_offset
            .checked_add(old_kernel_size)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "kernel offset overflows"))?,
        PAGE_SIZE,
    )?;
    let old_tail_offset = align_up_checked(
        old_ramdisk_offset
            .checked_add(ramdisk_size)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "ramdisk offset overflows")
            })?,
        PAGE_SIZE,
    )?;
    if old_tail_offset > template_bytes.len()
        || old_ramdisk_offset
            .checked_add(ramdisk_size)
            .is_none_or(|end| end > template_bytes.len())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "boot template has truncated kernel or ramdisk payload",
        ));
    }

    let kernel_bytes = fs::read(kernel)?;
    let kernel_size = u32::try_from(kernel_bytes.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "generated kernel exceeds Android v3 size",
        )
    })?;
    let mut header = template_bytes[..PAGE_SIZE].to_vec();
    write_le_u32(&mut header, KERNEL_SIZE_OFFSET, kernel_size)?;

    let mut image = header;
    append_padded(&mut image, &kernel_bytes, PAGE_SIZE)?;
    append_padded(
        &mut image,
        &template_bytes[old_ramdisk_offset..old_ramdisk_offset + ramdisk_size],
        PAGE_SIZE,
    )?;
    image.extend_from_slice(&template_bytes[old_tail_offset..]);
    fs::write(output, image)?;
    Ok(())
}

fn audit_bramble_boot_image(template: &Path, kernel: &Path, output: &Path) -> io::Result<()> {
    const PAGE_SIZE: usize = 4096;
    const KERNEL_SIZE_OFFSET: usize = 8;
    const RAMDISK_SIZE_OFFSET: usize = 12;
    const HEADER_SIZE_OFFSET: usize = 20;
    const HEADER_VERSION_OFFSET: usize = 40;

    let template_storage = fs::read(template)?;
    let template_bytes = strip_avb_metadata(&template_storage)?;
    let output_bytes = fs::read(output)?;
    let kernel_bytes = fs::read(kernel)?;
    if template_bytes.len() < PAGE_SIZE
        || output_bytes.len() < PAGE_SIZE
        || &template_bytes[..8] != b"ANDROID!"
        || &output_bytes[..8] != b"ANDROID!"
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "boot audit found a missing Android header",
        ));
    }
    if read_le_u32(&template_bytes, HEADER_VERSION_OFFSET)? != 3
        || read_le_u32(&output_bytes, HEADER_VERSION_OFFSET)? != 3
        || read_le_u32(&output_bytes, HEADER_SIZE_OFFSET)? == 0
        || read_le_u32(&output_bytes, HEADER_SIZE_OFFSET)? as usize > PAGE_SIZE
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "boot audit requires an Android v3 header within the first page",
        ));
    }

    let old_kernel_size = read_le_u32(&template_bytes, KERNEL_SIZE_OFFSET)? as usize;
    let ramdisk_size = read_le_u32(&template_bytes, RAMDISK_SIZE_OFFSET)? as usize;
    let kernel_size = read_le_u32(&output_bytes, KERNEL_SIZE_OFFSET)? as usize;
    if kernel_size != kernel_bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "boot audit kernel size is {kernel_size}, expected {}",
                kernel_bytes.len()
            ),
        ));
    }
    if read_le_u32(&output_bytes, RAMDISK_SIZE_OFFSET)? as usize != ramdisk_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "boot audit changed the ramdisk size",
        ));
    }
    let mut expected_header = template_bytes[..PAGE_SIZE].to_vec();
    write_le_u32(
        &mut expected_header,
        KERNEL_SIZE_OFFSET,
        u32::try_from(kernel_bytes.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "boot audit kernel is too large")
        })?,
    )?;
    if output_bytes[..PAGE_SIZE] != expected_header {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "boot audit header differs from the template outside kernel size",
        ));
    }

    let old_ramdisk_offset = align_up_checked(
        PAGE_SIZE.checked_add(old_kernel_size).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "boot audit kernel offset overflows",
            )
        })?,
        PAGE_SIZE,
    )?;
    let old_tail_offset = align_up_checked(
        old_ramdisk_offset
            .checked_add(ramdisk_size)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "boot audit overflows"))?,
        PAGE_SIZE,
    )?;
    if old_tail_offset > template_bytes.len()
        || old_ramdisk_offset + ramdisk_size > template_bytes.len()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "boot audit template payload is truncated",
        ));
    }

    let new_ramdisk_offset = align_up_checked(
        PAGE_SIZE
            .checked_add(kernel_bytes.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "boot audit overflows"))?,
        PAGE_SIZE,
    )?;
    let new_tail_offset = align_up_checked(
        new_ramdisk_offset
            .checked_add(ramdisk_size)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "boot audit overflows"))?,
        PAGE_SIZE,
    )?;
    if new_tail_offset > output_bytes.len()
        || PAGE_SIZE + kernel_bytes.len() > output_bytes.len()
        || new_ramdisk_offset + ramdisk_size > output_bytes.len()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "boot audit output payload is truncated",
        ));
    }
    if &output_bytes[PAGE_SIZE..PAGE_SIZE + kernel_bytes.len()] != kernel_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "boot audit kernel payload differs from the generated payload",
        ));
    }
    if output_bytes[PAGE_SIZE + kernel_bytes.len()..new_ramdisk_offset]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "boot audit found non-zero kernel alignment padding",
        ));
    }
    if &output_bytes[new_ramdisk_offset..new_ramdisk_offset + ramdisk_size]
        != &template_bytes[old_ramdisk_offset..old_ramdisk_offset + ramdisk_size]
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "boot audit ramdisk differs from the stock template",
        ));
    }
    if output_bytes[new_ramdisk_offset + ramdisk_size..new_tail_offset]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "boot audit found non-zero ramdisk alignment padding",
        ));
    }
    if &output_bytes[new_tail_offset..] != &template_bytes[old_tail_offset..] {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "boot audit tail differs from the stock template",
        ));
    }
    println!(
        "Bramble boot audit: PASS (kernel={} bytes, ramdisk={} bytes, tail={} bytes)",
        kernel_bytes.len(),
        ramdisk_size,
        template_bytes.len() - old_tail_offset
    );
    Ok(())
}

fn audit_android_boot_image(image: &Path) -> io::Result<()> {
    const PAGE_SIZE: usize = 4096;
    const KERNEL_SIZE_OFFSET: usize = 8;
    const RAMDISK_SIZE_OFFSET: usize = 12;
    const HEADER_SIZE_OFFSET: usize = 20;
    const HEADER_VERSION_OFFSET: usize = 40;

    let storage = fs::read(image)?;
    let bytes = strip_avb_metadata(&storage)?;
    if bytes.len() < PAGE_SIZE || &bytes[..8] != b"ANDROID!" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Fastboot image audit: missing Android boot header",
        ));
    }
    if read_le_u32(bytes, HEADER_VERSION_OFFSET)? != 3 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Fastboot image audit: only Android boot header v3 is supported",
        ));
    }
    let header_size = read_le_u32(bytes, HEADER_SIZE_OFFSET)? as usize;
    if header_size == 0 || header_size > PAGE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Fastboot image audit: invalid v3 header size",
        ));
    }
    let kernel_size = read_le_u32(bytes, KERNEL_SIZE_OFFSET)? as usize;
    let ramdisk_size = read_le_u32(bytes, RAMDISK_SIZE_OFFSET)? as usize;
    let ramdisk_offset = align_up_checked(
        PAGE_SIZE.checked_add(kernel_size).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "Fastboot image audit overflow")
        })?,
        PAGE_SIZE,
    )?;
    let tail_offset = align_up_checked(
        ramdisk_offset.checked_add(ramdisk_size).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "Fastboot image audit overflow")
        })?,
        PAGE_SIZE,
    )?;
    if tail_offset > bytes.len() || ramdisk_offset + ramdisk_size > bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Fastboot image audit: kernel or ramdisk exceeds image",
        ));
    }
    println!(
        "Fastboot image audit: PASS (kernel={} bytes, ramdisk={} bytes, tail={} bytes)",
        kernel_size,
        ramdisk_size,
        bytes.len() - tail_offset
    );
    Ok(())
}

fn strip_avb_metadata(image: &[u8]) -> io::Result<&[u8]> {
    const AVB_FOOTER_SIZE: usize = 64;
    if image.len() < AVB_FOOTER_SIZE
        || &image[image.len() - AVB_FOOTER_SIZE..image.len() - AVB_FOOTER_SIZE + 4] != b"AVBf"
    {
        return Ok(image);
    }

    let footer = &image[image.len() - AVB_FOOTER_SIZE..];
    let version_major = u32::from_be_bytes(footer[4..8].try_into().unwrap());
    let version_minor = u32::from_be_bytes(footer[8..12].try_into().unwrap());
    if version_major != 1 || version_minor != 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("unsupported AVB footer version {version_major}.{version_minor}"),
        ));
    }
    let original_image_size = u64::from_be_bytes(footer[12..20].try_into().unwrap());
    let vbmeta_offset = u64::from_be_bytes(footer[20..28].try_into().unwrap());
    let vbmeta_size = u64::from_be_bytes(footer[28..36].try_into().unwrap());
    let footer_offset = image.len() - AVB_FOOTER_SIZE;
    let original_image_size = usize::try_from(original_image_size).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "AVB original image size overflows host",
        )
    })?;
    let vbmeta_offset = usize::try_from(vbmeta_offset).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "AVB vbmeta offset overflows host",
        )
    })?;
    let vbmeta_size = usize::try_from(vbmeta_size).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "AVB vbmeta size overflows host")
    })?;
    if original_image_size > footer_offset
        || vbmeta_offset > footer_offset
        || vbmeta_size > footer_offset - vbmeta_offset
        || vbmeta_offset + vbmeta_size > footer_offset
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "AVB footer points outside the boot image",
        ));
    }
    Ok(&image[..original_image_size])
}

fn read_le_u32(bytes: &[u8], offset: usize) -> io::Result<u32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "integer overflow"))?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "boot header is truncated"))?;
    Ok(u32::from_le_bytes(value.try_into().unwrap()))
}

fn read_u32(bytes: &[u8], offset: usize) -> io::Result<u32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "integer overflow"))?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "buffer is truncated"))?;
    Ok(u32::from_le_bytes(value.try_into().unwrap()))
}

fn read_u64(bytes: &[u8], offset: usize) -> io::Result<u64> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "integer overflow"))?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "buffer is truncated"))?;
    Ok(u64::from_le_bytes(value.try_into().unwrap()))
}

fn write_le_u32(bytes: &mut [u8], offset: usize, value: u32) -> io::Result<()> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "integer overflow"))?;
    let target = bytes
        .get_mut(offset..end)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "boot header is truncated"))?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn align_up_checked(value: usize, alignment: usize) -> io::Result<usize> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "alignment overflows"))
}

fn append_padded(output: &mut Vec<u8>, payload: &[u8], alignment: usize) -> io::Result<()> {
    output.extend_from_slice(payload);
    let padded_len = align_up_checked(payload.len(), alignment)?;
    output.resize(output.len() + padded_len - payload.len(), 0);
    Ok(())
}

fn aarch64_qemu_args(
    artifact: &Path,
    platform: Platform,
    debug: bool,
    qemu_usb_sim: bool,
) -> io::Result<Vec<String>> {
    if platform != Platform::QemuVirt {
        platform.validate(Arch::Aarch64, Action::Run)?;
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
    if qemu_usb_sim {
        args.extend([
            "-semihosting-config".to_string(),
            "enable=on,target=native".to_string(),
        ]);
    }
    Ok(args)
}

fn run_aarch64_qemu(
    artifact: &Path,
    platform: Platform,
    debug: bool,
    timeout: Option<u64>,
    qemu_usb_sim: bool,
) -> io::Result<()> {
    let qemu_dtb = if platform == Platform::QemuVirt {
        Some(TemporaryQemuDtb::create()?)
    } else {
        None
    };
    let mut qemu = Command::new(platform.qemu_binary());
    let mut qemu_args = aarch64_qemu_args(artifact, platform, debug, qemu_usb_sim)?;
    if let Some(dtb) = qemu_dtb.as_ref() {
        qemu_args.extend(["-dtb".to_string(), dtb.path.display().to_string()]);
    }
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

/// QEMU's `-kernel` path does not reliably provide its generated DTB in x0
/// for a freestanding ELF. Dump the machine's own DTB first, then explicitly
/// pass it to the real run. This keeps QEMU on the same DTB discovery path as
/// an Android bootloader and prevents the platform defaults from hiding
/// parser or address-cell bugs.
struct TemporaryQemuDtb {
    path: PathBuf,
}

impl TemporaryQemuDtb {
    fn create() -> io::Result<Self> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| {
                io::Error::other(format!("system clock is before UNIX epoch: {error}"))
            })?
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "fullerene-qemu-virt-{}-{nonce}.dtb",
            std::process::id()
        ));
        let machine = format!("virt,gic-version=3,dumpdtb={}", path.display());
        let status = Command::new("qemu-system-aarch64")
            .args([
                "-M",
                &machine,
                "-cpu",
                "cortex-a72",
                "-m",
                "1G",
                "-nographic",
            ])
            .status()?;
        if !status.success() {
            return Err(io::Error::other(
                "qemu-system-aarch64 could not generate a virt device tree",
            ));
        }
        let size = fs::metadata(&path)?.len();
        if size < 40 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("QEMU generated an invalid DTB at {}", path.display()),
            ));
        }
        Ok(Self { path })
    }
}

impl Drop for TemporaryQemuDtb {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
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
    use super::{
        Action, Arch, Args, BuildProfile, Platform, aarch64_qemu_args, audit_aarch64_image_bytes,
        audit_android_boot_image, audit_bramble_boot_image, decode_literal_lz4_frame,
        make_aarch64_image, make_lz4_frame, patch_bramble_boot_image, strip_avb_metadata, xxhash32,
    };
    use clap::Parser;
    use std::{fs, path::Path};
    use tempfile::tempdir;

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
    fn aarch64_uses_the_fullerene_kernel_arch_target() {
        assert_eq!(Arch::Aarch64.cargo_package(), "fullerene-kernel");
        assert_eq!(Arch::Aarch64.kernel_artifact(), "fullerene-kernel-aarch64");
    }

    #[test]
    fn aarch64_qemu_command_uses_virt_and_kernel_artifact() {
        let args = aarch64_qemu_args(
            Path::new("target/aarch64-unknown-none/release/fullerene-kernel-aarch64"),
            Platform::QemuVirt,
            false,
            false,
        )
        .unwrap();
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "-M" && pair[1] == "virt,gic-version=3")
        );
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "-cpu" && pair[1] == "cortex-a72")
        );
        assert!(args.iter().any(|arg| arg == "-nographic"));
        assert!(
            args.iter()
                .any(|arg| arg.ends_with("fullerene-kernel-aarch64"))
        );
    }

    #[test]
    fn bramble_run_target_is_distinct_from_aarch64_qemu() {
        let args = Args::try_parse_from([
            "flasks",
            "run",
            "--arch",
            "aarch64",
            "--platform",
            "bramble",
        ])
        .unwrap();
        let target = super::Target::from_args(&args).unwrap();
        assert_eq!(target.arch, Arch::Aarch64);
        assert_eq!(target.platform, Platform::Bramble);
    }

    #[test]
    fn bramble_build_is_allowed_to_produce_a_raw_kernel() {
        let args = Args::try_parse_from([
            "flasks",
            "build",
            "--arch",
            "aarch64",
            "--platform",
            "bramble",
        ])
        .unwrap();
        let target = super::Target::from_args(&args).unwrap();
        assert_eq!(target.platform, Platform::Bramble);
    }

    #[test]
    fn bramble_boot_template_replaces_kernel_and_preserves_ramdisk() {
        let directory = tempdir().unwrap();
        let template = directory.path().join("boot.img");
        let kernel = directory.path().join("Image.lz4");
        let output = directory.path().join("patched-boot.img");

        let mut boot = vec![0u8; 4096];
        boot[..8].copy_from_slice(b"ANDROID!");
        boot[8..12].copy_from_slice(&3u32.to_le_bytes());
        boot[12..16].copy_from_slice(&5u32.to_le_bytes());
        boot[20..24].copy_from_slice(&1580u32.to_le_bytes());
        boot[40..44].copy_from_slice(&3u32.to_le_bytes());
        boot.extend_from_slice(&[0xaa, 0xbb, 0xcc]);
        boot.resize(8192, 0);
        boot.extend_from_slice(b"ramfs");
        boot.resize(12288, 0);
        boot.extend_from_slice(b"tail");
        fs::write(&template, boot).unwrap();
        fs::write(&kernel, b"new Image.lz4").unwrap();

        patch_bramble_boot_image(&template, &kernel, &output).unwrap();
        audit_bramble_boot_image(&template, &kernel, &output).unwrap();
        audit_android_boot_image(&output).unwrap();
        let patched = fs::read(&output).unwrap();
        assert_eq!(&patched[..8], b"ANDROID!");
        assert_eq!(u32::from_le_bytes(patched[8..12].try_into().unwrap()), 13);
        assert_eq!(&patched[4096..4109], b"new Image.lz4");
        assert_eq!(&patched[8192..8197], b"ramfs");
        assert_eq!(&patched[12288..], b"tail");
    }

    #[test]
    fn boot_audit_rejects_kernel_payload_corruption() {
        let directory = tempdir().unwrap();
        let template = directory.path().join("boot.img");
        let kernel = directory.path().join("Image.lz4");
        let output = directory.path().join("patched-boot.img");

        let mut boot = vec![0u8; 4096];
        boot[..8].copy_from_slice(b"ANDROID!");
        boot[12..16].copy_from_slice(&5u32.to_le_bytes());
        boot[20..24].copy_from_slice(&3u32.to_le_bytes());
        boot[40..44].copy_from_slice(&3u32.to_le_bytes());
        boot.resize(8192, 0);
        boot.extend_from_slice(b"ramfs");
        boot.resize(12288, 0);
        fs::write(&template, boot).unwrap();
        fs::write(&kernel, b"kernel").unwrap();

        patch_bramble_boot_image(&template, &kernel, &output).unwrap();
        let mut corrupted = fs::read(&output).unwrap();
        corrupted[4096] ^= 1;
        fs::write(&output, corrupted).unwrap();
        assert!(audit_bramble_boot_image(&template, &kernel, &output).is_err());
    }

    #[test]
    fn avb_footer_is_removed_before_boot_image_patching() {
        let mut image = vec![0x5a; 8192];
        let footer_offset = image.len();
        image.extend_from_slice(&[0xa5; 128]);
        let mut footer = [0u8; 64];
        footer[..4].copy_from_slice(b"AVBf");
        footer[4..8].copy_from_slice(&1u32.to_be_bytes());
        footer[8..12].copy_from_slice(&0u32.to_be_bytes());
        footer[12..20].copy_from_slice(&(footer_offset as u64).to_be_bytes());
        footer[20..28].copy_from_slice(&(footer_offset as u64).to_be_bytes());
        footer[28..36].copy_from_slice(&(128u64).to_be_bytes());
        image.extend_from_slice(&footer);

        let stripped = strip_avb_metadata(&image).unwrap();
        assert_eq!(stripped.len(), footer_offset);
        assert_eq!(stripped, &[0x5a; 8192]);
    }

    #[test]
    fn boot_patcher_rejects_avb_footer_with_too_small_original_image() {
        let directory = tempdir().unwrap();
        let template = directory.path().join("boot.img");
        let kernel = directory.path().join("Image.lz4");
        let output = directory.path().join("patched-boot.img");

        let mut image = vec![0u8; 4096];
        image[..8].copy_from_slice(b"ANDROID!");
        image[40..44].copy_from_slice(&3u32.to_le_bytes());
        image.extend_from_slice(&[0xa5; 128]);
        let mut footer = [0u8; 64];
        footer[..4].copy_from_slice(b"AVBf");
        footer[4..8].copy_from_slice(&1u32.to_be_bytes());
        footer[12..20].copy_from_slice(&(32u64).to_be_bytes());
        footer[20..28].copy_from_slice(&(4096u64).to_be_bytes());
        footer[28..36].copy_from_slice(&(128u64).to_be_bytes());
        image.extend_from_slice(&footer);
        fs::write(&template, image).unwrap();
        fs::write(&kernel, b"kernel").unwrap();

        let error = patch_bramble_boot_image(&template, &kernel, &output).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn aarch64_image_has_branch_header_and_arm64_magic() {
        let image = make_aarch64_image(&[0xaa, 0xbb, 0xcc, 0xdd]);
        assert_eq!(
            u32::from_le_bytes(image[0..4].try_into().unwrap()),
            0x1400_0010
        );
        assert_eq!(&image[56..60], b"ARMd");
        assert_eq!(&image[64..], &[0xaa, 0xbb, 0xcc, 0xdd]);
        assert_eq!(
            u64::from_le_bytes(image[8..16].try_into().unwrap()),
            0x80000
        );
        assert_eq!(u64::from_le_bytes(image[24..32].try_into().unwrap()), 0x02);
    }

    #[test]
    fn lz4_frame_uses_literal_blocks_and_checksums() {
        let payload = b"fullerene-aarch64";
        let frame = make_lz4_frame(payload);
        assert_eq!(&frame[0..4], &[0x04, 0x22, 0x4d, 0x18]);
        assert_eq!(&frame[4..6], &[0x64, 0x70]);
        assert_eq!(frame[6], (xxhash32(&frame[4..6], 0) >> 8) as u8);
        let block_size = u32::from_le_bytes(frame[7..11].try_into().unwrap()) as usize;
        assert_eq!(block_size, payload.len() + 2);
        assert_eq!(frame[11] >> 4, 15);
        assert_eq!(frame[12], (payload.len() - 15) as u8);
        assert_eq!(&frame[13..13 + payload.len()], payload);
        assert_eq!(
            &frame[13 + payload.len()..17 + payload.len()],
            &[0, 0, 0, 0]
        );
        assert_eq!(
            &frame[17 + payload.len()..],
            &xxhash32(payload, 0).to_le_bytes()
        );
        assert_eq!(decode_literal_lz4_frame(&frame).unwrap(), payload);
    }

    #[test]
    fn lz4_audit_rejects_a_modified_content_checksum() {
        let mut frame = make_lz4_frame(b"fullerene-aarch64");
        let last = frame.len() - 1;
        frame[last] ^= 1;
        assert!(decode_literal_lz4_frame(&frame).is_err());
    }

    #[test]
    fn aarch64_image_audit_checks_payload_and_header() {
        let raw = [0xaa, 0xbb, 0xcc, 0xdd];
        let image = make_aarch64_image(&raw);
        audit_aarch64_image_bytes(&image, &raw).unwrap();

        let mut corrupted = image;
        corrupted[56] = 0;
        assert!(audit_aarch64_image_bytes(&corrupted, &raw).is_err());
    }

    #[test]
    fn lz4_frame_keeps_each_encoded_block_within_bd_limit() {
        let payload = vec![0x5a; 4 * 1024 * 1024 + 1024];
        let frame = make_lz4_frame(&payload);
        let mut cursor = 7;
        let mut blocks = 0;
        loop {
            let size = u32::from_le_bytes(frame[cursor..cursor + 4].try_into().unwrap()) as usize;
            cursor += 4;
            if size == 0 {
                break;
            }
            assert!(size <= 4 * 1024 * 1024);
            cursor += size;
            blocks += 1;
        }
        assert!(blocks >= 2);
    }
}
