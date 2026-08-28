//! Rust replacement for the Bramble USB handoff shell harness.
//!
//! The harness deliberately delegates image construction and the actual
//! Fastboot protocol to Flasks, so the safety boundary stays in one place:
//! the only device-side image operation is `fastboot boot`.

use clap::{Parser, Subcommand, ValueEnum};
use nusb::transfer::{ControlIn, ControlType, Recipient};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use tokio::runtime::Builder;

const DEFAULT_SERIAL: &str = "26191JECB00076";
const DEFAULT_TEMPLATE: &str = "/tmp/fullerene-stock-template.Uvg3m2/boot.img";
const BOOTLOADER_USB: &str = "18d1:4ee0";
const ANDROID_FALLBACK_USB: &str = "18d1:4ee7";
const FULLERENE_USB: &str = "1234:0001";
// Gate runs read the gate bit from the handset's return timing: a false gate
// parks for 90 s before resetting, so the recovery wait must cover the park
// plus the Android boot (well beyond 75 s).
const RECOVERY_TIMEOUT_SECS: u64 = 150;

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
    /// Read the retained post-mortem USB trace from an enumerated Fullerene gadget.
    Trace(TraceArgs),
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
    /// Run the normal non-destructive handoff first, with the probe's
    /// retained-trace watchdog and automatic recovery still enabled.
    #[arg(long)]
    direct_handoff: bool,
    #[arg(long)]
    pullup_only: bool,
    /// Run the minimal USB2 pull-up sequence without DWC3 reset, DMA, or EP0.
    #[arg(long)]
    bare_pullup: bool,
    /// Publish only the physical pull-up after one gadget handoff boundary
    /// (1..=12), then use the normal automatic recovery path.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=12))]
    stop_after_stage: Option<u32>,
    #[arg(long)]
    no_smmu: bool,
    /// Reuse Fastboot's event-ring DMA page instead of the linker-reserved
    /// EP0 DMA objects. This is a Rust-only firmware-mapping differential.
    #[arg(long)]
    reuse_fastboot_dma: bool,
    #[arg(long)]
    no_transfer_resource: bool,
    #[arg(long)]
    android_resource_order: bool,
    /// Arm EP0 STARTTRANSFER immediately after Run/Stop (Bramble A/B).
    #[arg(long)]
    start_after_connect: bool,
    /// Arm the initial EP0 SETUP only after the host USB Reset event.
    #[arg(long)]
    start_after_reset: bool,
    /// Arm the initial EP0 SETUP from the DWC3 Connect Done event.
    #[arg(long)]
    start_at_connect_done: bool,
    /// Publish EP0/event/TRB diagnostics by dropping the pull-up at a coded
    /// delay after attach; the host dmesg delta is the readout.
    #[arg(long)]
    signal_probe: bool,
    /// Include the read-only Apps-SMMU SMR/S2CR stream state in the signal
    /// probe (priority over the runtime signal codes).
    #[arg(long)]
    signal_smmu_state: bool,
    /// Switch the signal probe to the USB2 link-state ladder.
    #[arg(long)]
    signal_link_state: bool,
    /// Encode the raw DSTS.USBLNKST nibble at 2-second resolution.
    #[arg(long)]
    signal_raw_link: bool,
    /// Drop the pull-up permanently in the handoff when the selected early
    /// condition (1/2/3/5, or 9=unconditional control) is observed.
    #[arg(long = "signal-early-drop", value_name = "CODE")]
    signal_early_drop: Option<u32>,
    /// Drop the session overrides before the first Run/Stop (control).
    #[arg(long)]
    signal_pre_drop: bool,
    /// Toggle DCTL Run/Stop at one-second intervals after the connect.
    #[arg(long)]
    signal_heartbeat: bool,
    /// Adopt the bootloader's mapped SMMU page for the EP0 DMA objects.
    #[arg(long)]
    dma_adopt_smmu: bool,
    /// Publish the pull-up only when the SMMU stream's S2CR type matches.
    #[arg(long = "smmu-gate", value_name = "TYPE")]
    smmu_gate: Option<u32>,
    /// Drop the pull-up via the QUSB2 VBUSVLDEXT0 session bits too.
    #[arg(long)]
    signal_drop_vbusvld: bool,
    /// Delay only the first attempt's Run/Stop by this many seconds.
    #[arg(long = "connect-delay", value_name = "SECS")]
    connect_delay: Option<u64>,
    /// Claim a free SMMU SMR (S2CR BYPASS, readback-verified) for the DWC3
    /// stream before Run/Stop; the attach gates on the install.
    #[arg(long)]
    smmu_install_bypass: bool,
    /// Gate the attach on a pre-connect CMDIOC event reaching GEVNTCOUNT.
    #[arg(long)]
    signal_dma_probe: bool,
    /// Install the SMR as a catch-all (mask all IDs) instead of exact 0xe0.
    #[arg(long)]
    smmu_install_all: bool,
    /// FSR gate: 1 = attach only when the SMMU faulted during the probe.
    #[arg(long = "signal-fsr-gate", value_name = "MODE")]
    signal_fsr_gate: Option<u32>,
    /// Gate the attach on a CPU readback of the .usb_dma region succeeding.
    #[arg(long)]
    signal_ram_gate: bool,
    /// Skip the SPMI Type-C handoff observation at probe entry (timing A/B).
    #[arg(long)]
    skip_typec_spmi: bool,
    /// Publish the pull-up even when the handoff failed (read pre-Run/Stop
    /// gates via the attach presence).
    #[arg(long)]
    signal_diag_publish: bool,
    /// Stop all controller MMIO access N seconds after the first Run/Stop.
    #[arg(long = "quiet-after", value_name = "SECS")]
    quiet_after: Option<u64>,
    /// Signal-probe observation window (seconds) before the gate is
    /// evaluated; keep it short enough to beat the ~17 s watchdog.
    #[arg(long = "observe-secs", value_name = "SECS")]
    observe_secs: Option<u64>,
    /// Relocate the .usb_dma section to this hex address for the run.
    #[arg(long = "dma-origin", value_name = "ADDR")]
    dma_origin: Option<String>,
    /// Gate the attach on the previous attempt's STARTTRANSFER outcome.
    #[arg(long = "signal-cmd-gate", value_name = "WHEN")]
    signal_cmd_gate: Option<String>,
    /// Gate on the previous SETTRANSFRESOURCE raw DEPCMD register.
    #[arg(long = "signal-rsc-gate", value_name = "RAW")]
    signal_rsc_gate: Option<String>,
    /// Gate on the previous DEPSTARTCFG raw DEPCMD register.
    #[arg(long = "signal-cfg-gate", value_name = "RAW")]
    signal_cfg_gate: Option<String>,
    /// Gate on the captured GCTL.RAMCLKSEL value (0..=3).
    #[arg(long = "signal-ramclk-gate", value_name = "VALUE")]
    signal_ramclk_gate: Option<u32>,
    /// Clear sCR0.SMMUEN/WACFG (readback-verified) before any DWC3 DMA.
    #[arg(long)]
    smmu_disable: bool,
    /// Gate on the probe event word landing in DRAM (1 = landed).
    #[arg(long = "signal-evt-data-gate", value_name = "MODE")]
    signal_evt_data_gate: Option<u32>,
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

#[derive(Parser, Debug)]
struct TraceArgs {
    /// Require a specific Fullerene device serial from the USB descriptor.
    #[arg(long)]
    serial: Option<String>,
    /// Maximum time for each vendor control transfer.
    #[arg(long, default_value_t = 2)]
    timeout: u64,
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
        CommandKind::Trace(args) => run_trace(args),
    }
}

fn run_trace(args: TraceArgs) -> io::Result<()> {
    if args.timeout == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--timeout must be greater than zero",
        ));
    }
    Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| io::Error::other(error.to_string()))?
        .block_on(read_trace(args))
}

async fn read_trace(args: TraceArgs) -> io::Result<()> {
    let devices = nusb::list_devices()
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    let devices: Vec<_> = devices
        .filter(|device| device.vendor_id() == 0x1234 && device.product_id() == 0x0001)
        .filter(|device| {
            args.serial
                .as_deref()
                .is_none_or(|serial| device.serial_number().as_deref() == Some(serial))
        })
        .collect();
    let info = match devices.as_slice() {
        [] => {
            return Err(io::Error::other(
                "no Fullerene USB gadget (1234:0001) found",
            ));
        }
        [info] => info,
        many => {
            return Err(io::Error::other(format!(
                "refusing to choose between {} Fullerene USB gadgets",
                many.len()
            )));
        }
    };
    println!(
        "trace device: bus={} address={} serial={}",
        info.bus_id(),
        info.device_address(),
        info.serial_number().unwrap_or_default()
    );
    let device = info
        .open()
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    let transfer_timeout = Duration::from_secs(args.timeout);
    let first = trace_page(&device, 0, transfer_timeout).await?;
    let header = parse_trace_header(&first)?;
    println!(
        "trace header: magic=FUTR version={} head={} valid={}",
        header.version, header.head, header.valid
    );
    let pages = (header.valid as usize).div_ceil(TRACE_PAGE_ENTRIES);
    for page in 0..pages.max(1) {
        let response = if page == 0 {
            first.as_slice()
        } else {
            // Keep the buffer alive until all records on this page have been
            // printed; the request is deliberately a bounded 512-byte read.
            let response = trace_page(&device, page as u16, transfer_timeout).await?;
            print_trace_page(page, &response, header.valid as usize)?;
            continue;
        };
        print_trace_page(page, response, header.valid as usize)?;
    }
    Ok(())
}

const TRACE_REQUEST: u8 = 0x5a;
const TRACE_PAGE_BYTES: usize = 512;
const TRACE_HEADER_BYTES: usize = 16;
const TRACE_ENTRY_BYTES: usize = 32;
const TRACE_PAGE_ENTRIES: usize = 15;
const TRACE_MAGIC: u32 = 0x4655_5452;
const TRACE_VERSION: u32 = 1;

struct TraceHeader {
    version: u32,
    head: u32,
    valid: u32,
}

async fn trace_page(device: &nusb::Device, page: u16, timeout: Duration) -> io::Result<Vec<u8>> {
    device
        .control_in(
            ControlIn {
                control_type: ControlType::Vendor,
                recipient: Recipient::Device,
                request: TRACE_REQUEST,
                value: page,
                index: 0,
                length: TRACE_PAGE_BYTES as u16,
            },
            timeout,
        )
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

fn parse_trace_header(response: &[u8]) -> io::Result<TraceHeader> {
    if response.len() < TRACE_HEADER_BYTES {
        return Err(io::Error::other(
            "trace response is shorter than its header",
        ));
    }
    let magic = trace_word(response, 0);
    let version = trace_word(response, 4);
    let head = trace_word(response, 8);
    let valid = trace_word(response, 12);
    if magic != TRACE_MAGIC || version != TRACE_VERSION || valid > 256 {
        return Err(io::Error::other("invalid retained trace header"));
    }
    Ok(TraceHeader {
        version,
        head,
        valid,
    })
}

fn print_trace_page(page: usize, response: &[u8], valid: usize) -> io::Result<()> {
    let header = parse_trace_header(response)?;
    let page_start = page
        .checked_mul(TRACE_PAGE_ENTRIES)
        .ok_or_else(|| io::Error::other("trace page index overflow"))?;
    let records = response.len().saturating_sub(TRACE_HEADER_BYTES) / TRACE_ENTRY_BYTES;
    let records = records
        .min(TRACE_PAGE_ENTRIES)
        .min(valid.saturating_sub(page_start));
    for index in 0..records {
        let base = TRACE_HEADER_BYTES + index * TRACE_ENTRY_BYTES;
        let values: Vec<_> = (0..8)
            .map(|word| trace_word(&response[base..base + TRACE_ENTRY_BYTES], word * 4))
            .collect();
        println!(
            "trace page={} index={} sequence={} event=0x{:08x} request=0x{:08x} value=0x{:08x} index=0x{:08x} length={} ep0_state={} status=0x{:08x}",
            page,
            index,
            values[0],
            values[1],
            values[2],
            values[3],
            values[4],
            values[5],
            values[6],
            values[7]
        );
    }
    if header.valid as usize != valid {
        return Err(io::Error::other("trace header changed during read"));
    }
    Ok(())
}

fn trace_word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
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

    let run_dir = create_run_dir(workspace, "fullerene-bramble-matrix")?;
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
                if let Err(recovery) = restore_fastboot(&args.serial, args.fastboot_wait) {
                    return Err(io::Error::other(format!(
                        "route {} failed and Rust recovery to Fastboot failed: {recovery}; logs are under {}",
                        route.as_str(),
                        run_dir.display()
                    )));
                }
                eprintln!("recovered to Fastboot; trying the next route");
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
        direct_handoff: false,
        pullup_only: false,
        no_smmu: args.no_smmu,
        reuse_fastboot_dma: false,
        no_transfer_resource: false,
        android_resource_order: false,
        start_after_connect: false,
        start_after_reset: false,
        start_at_connect_done: false,
        signal_probe: false,
        signal_smmu_state: false,
        signal_link_state: false,
        signal_raw_link: false,
        signal_early_drop: None,
        signal_pre_drop: false,
        signal_heartbeat: false,
        dma_adopt_smmu: false,
        smmu_gate: None,
        signal_drop_vbusvld: false,
        connect_delay: None,
        smmu_install_bypass: false,
        signal_dma_probe: false,
        smmu_install_all: false,
        signal_fsr_gate: None,
        signal_ram_gate: false,
        skip_typec_spmi: false,
        signal_diag_publish: false,
        quiet_after: None,
        observe_secs: None,
        dma_origin: None,
        signal_cmd_gate: None,
        signal_rsc_gate: None,
        signal_cfg_gate: None,
        signal_ramclk_gate: None,
        smmu_disable: false,
        signal_evt_data_gate: None,
        no_core_reset: args.no_core_reset,
        uncompressed: false,
        dry_run: false,
        bare_pullup: false,
        stop_after_stage: None,
    }
}

fn run_loop(workspace: &Path, args: LoopArgs) -> io::Result<()> {
    if args.normal
        && (args.super_speed
            || args.pullup_only
            || args.bare_pullup
            || args.stop_after_stage.is_some()
            || args.direct_handoff)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--normal cannot be combined with --super-speed or --pullup-only",
        ));
    }
    if args.direct_handoff && (args.super_speed || args.pullup_only || args.bare_pullup) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--direct-handoff is only available for the USB2 gadget handoff probe",
        ));
    }
    if args.start_after_connect && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--start-after-connect requires --direct-handoff",
        ));
    }
    if args.start_after_reset && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--start-after-reset requires --direct-handoff",
        ));
    }
    if args.start_at_connect_done && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--start-at-connect-done requires --direct-handoff",
        ));
    }
    if args.signal_probe && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-probe requires --direct-handoff",
        ));
    }
    if args.signal_smmu_state && !args.signal_probe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-smmu-state requires --signal-probe",
        ));
    }
    if args.signal_link_state && !args.signal_probe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-link-state requires --signal-probe",
        ));
    }
    if args.signal_raw_link && !args.signal_probe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-raw-link requires --signal-probe",
        ));
    }
    if args.signal_early_drop.is_some() && !args.signal_probe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-early-drop requires --signal-probe",
        ));
    }
    if args.signal_pre_drop && !args.signal_probe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-pre-drop requires --signal-probe",
        ));
    }
    if args.signal_heartbeat && !args.signal_probe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-heartbeat requires --signal-probe",
        ));
    }
    if args.dma_adopt_smmu && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--dma-adopt-smmu requires --direct-handoff",
        ));
    }
    if args.smmu_gate.is_some() && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--smmu-gate requires --direct-handoff",
        ));
    }
    if args.signal_drop_vbusvld && !args.signal_probe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-drop-vbusvld requires --signal-probe",
        ));
    }
    if args.connect_delay.is_some() && !args.signal_probe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--connect-delay requires --signal-probe",
        ));
    }
    if args.smmu_install_bypass && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--smmu-install-bypass requires --direct-handoff",
        ));
    }
    if args.smmu_install_all && !args.smmu_install_bypass {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--smmu-install-all requires --smmu-install-bypass",
        ));
    }
    if args.signal_fsr_gate.is_some() && !args.signal_dma_probe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-fsr-gate requires --signal-dma-probe",
        ));
    }
    if args.signal_ram_gate && !args.signal_dma_probe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-ram-gate requires --signal-dma-probe",
        ));
    }
    if args.signal_diag_publish && !args.signal_probe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-diag-publish requires --signal-probe",
        ));
    }
    if args.quiet_after.is_some() && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--quiet-after requires --direct-handoff",
        ));
    }
    if args.dma_origin.is_some() && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--dma-origin requires --direct-handoff",
        ));
    }
    if args.signal_cmd_gate.is_some() && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-cmd-gate requires --direct-handoff",
        ));
    }
    if args.signal_rsc_gate.is_some() && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-rsc-gate requires --direct-handoff",
        ));
    }
    if args.signal_cfg_gate.is_some() && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-cfg-gate requires --direct-handoff",
        ));
    }
    if args.signal_ramclk_gate.is_some() && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-ramclk-gate requires --direct-handoff",
        ));
    }
    if args.smmu_disable && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--smmu-disable requires --direct-handoff",
        ));
    }
    if args.signal_evt_data_gate.is_some() && !args.signal_dma_probe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-evt-data-gate requires --signal-dma-probe",
        ));
    }
    if args.signal_dma_probe && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-dma-probe requires --direct-handoff",
        ));
    }
    if (args.start_after_connect as u8
        + args.start_after_reset as u8
        + args.start_at_connect_done as u8)
        > 1
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the EP0 timing differentials are mutually exclusive",
        ));
    }
    if args.pullup_only && (args.no_smmu || args.no_core_reset || args.irq_route.is_some()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--pullup-only cannot be combined with IRQ, SMMU, or core-reset differentials",
        ));
    }
    if args.bare_pullup
        && (args.pullup_only
            || args.super_speed
            || args.no_smmu
            || args.no_core_reset
            || args.irq_route.is_some())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--bare-pullup cannot be combined with another USB differential",
        ));
    }
    if args.reuse_fastboot_dma
        && (args.normal
            || args.super_speed
            || args.pullup_only
            || args.bare_pullup
            || args.no_core_reset
            || args.irq_route.is_some()
            || !args.no_smmu)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--reuse-fastboot-dma requires the USB2 gadget handoff with --no-smmu and cannot be combined with another differential",
        ));
    }
    if args.stop_after_stage.is_some()
        && (args.super_speed
            || args.pullup_only
            || args.bare_pullup
            || args.no_core_reset
            || args.irq_route.is_some())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--stop-after-stage cannot be combined with another USB differential",
        ));
    }
    if (args.no_smmu || args.no_transfer_resource) && args.normal {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SMMU/resource differentials require the gadget handoff probe",
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
        None => create_run_dir(workspace, "fullerene-bramble-loop")?,
    };
    let output = run_dir.join("fullerene-bramble-boot.img");
    println!("Bramble serial: {}", args.serial);
    println!("Stock template: {}", args.template.display());
    println!("Boot artifact: {}", output.display());
    println!("Logs: {}", run_dir.display());

    // A previous probe normally recovers through Android before the next
    // iteration. Make a standalone Rust loop just as self-contained as the
    // matrix: if Fastboot is not already visible, request the bootloader via
    // ADB. This is a reboot-only operation; it never flashes or erases.
    restore_fastboot(&args.serial, args.fastboot_wait)?;
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
    let boot_started = Instant::now();
    let boot_output = run_capture(&run_dir.join("boot.log"), &mut build_command_owned(boot))?;
    if !boot_output.status.success() {
        journal.save_final();
        return Err(io::Error::other("Fastboot boot failed"));
    }

    wait_until_absent(BOOTLOADER_USB, 15);
    let deadline = Instant::now() + Duration::from_secs(args.enum_timeout);
    let mut android_fallback = false;
    let mut timeline = File::create(run_dir.join("lsusb-timeline.txt"))?;
    while Instant::now() < deadline {
        // Record the full lsusb picture every second: an addressed-but-
        // unconfigured Fullerene device would still appear here with its
        // VID:PID, which distinguishes "enumeration broke after the device
        // descriptor read" from "the host never registered the device".
        let stamp =
            Instant::now().duration_since(deadline - Duration::from_secs(args.enum_timeout));
        let listing = Command::new("lsusb").output();
        if let Ok(output) = listing {
            let _ = writeln!(timeline, "[{:?}]", stamp);
            let _ = timeline.write_all(&output.stdout);
            let _ = timeline.flush();
        }
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
                // Gate readout: the seconds since `fastboot boot` separate the
                // buckets (gate fires ~10 s in; Android boot adds ~20 s):
                // ~35-45 s no gate ran / early reset, ~85-95 s gate TRUE
                // (60 s park), ~115-125 s gate FALSE (90 s park).
                println!(
                    "handset returned via Android after {} s",
                    boot_started.elapsed().as_secs()
                );
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
    if android_fallback {
        // The bootreason property is written by the bootloader from the PON
        // reset reason: it names what rebooted the handset mid-probe
        // (watchdog bite vs PS_HOLD release vs PSCI reboot) before the
        // restore step's own reboot overwrites it. ADB is not always
        // authenticated yet when Android first appears on the bus, so retry.
        for _ in 0..10 {
            let captured = capture_simple(
                &run_dir,
                "boot-reason",
                "adb",
                &["-s", &args.serial, "shell", "getprop", "ro.boot.bootreason"],
            );
            if let Ok(output) = captured {
                let text = String::from_utf8_lossy(&output.stdout);
                if output.status.success()
                    && !text.trim().is_empty()
                    && !text.contains("not found")
                    && !text.contains("error")
                {
                    break;
                }
            }
            thread::sleep(Duration::from_secs(2));
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
    } else if args.bare_pullup {
        "usb-bare-pullup-probe"
    } else if args.reuse_fastboot_dma {
        "usb-gadget-handoff-reuse-fastboot-dma"
    } else if args.direct_handoff {
        "usb-gadget-handoff-direct-probe"
    } else if args.stop_after_stage.is_some() {
        "usb-gadget-handoff-stage-probe"
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
        arguments.push(if args.bare_pullup {
            "--usb-bare-pullup-probe".to_owned()
        } else if args.pullup_only {
            "--usb-pullup-probe".to_owned()
        } else if args.super_speed {
            "--usb-gadget-handoff-super-speed-probe".to_owned()
        } else {
            "--usb-gadget-handoff-probe".to_owned()
        });
        if args.direct_handoff {
            arguments.push("--usb-gadget-handoff-direct".to_owned());
        }
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
    if args.reuse_fastboot_dma {
        arguments.push("--usb-gadget-handoff-reuse-fastboot-dma".to_owned());
    }
    if args.no_transfer_resource {
        arguments.push("--usb-gadget-handoff-no-transfer-resource".to_owned());
    }
    if args.android_resource_order {
        arguments.push("--usb-gadget-handoff-android-resource-order".to_owned());
    }
    if args.start_after_connect {
        arguments.push("--usb-gadget-handoff-start-after-connect".to_owned());
    }
    if args.signal_probe {
        arguments.push("--usb-ep0-signal-probe".to_owned());
    }
    if args.signal_smmu_state {
        arguments.push("--usb-ep0-signal-smmu-state".to_owned());
    }
    if args.signal_link_state {
        arguments.push("--usb-ep0-signal-link-state".to_owned());
    }
    if args.signal_raw_link {
        arguments.push("--usb-ep0-signal-raw-link".to_owned());
    }
    if let Some(code) = args.signal_early_drop {
        arguments.push("--usb-ep0-signal-early-drop".to_owned());
        arguments.push(code.to_string());
    }
    if args.signal_pre_drop {
        arguments.push("--usb-ep0-signal-pre-drop".to_owned());
    }
    if args.signal_heartbeat {
        arguments.push("--usb-ep0-signal-heartbeat".to_owned());
    }
    if args.dma_adopt_smmu {
        arguments.push("--usb-ep0-dma-adopt".to_owned());
    }
    if let Some(value) = args.smmu_gate {
        arguments.push("--usb-ep0-smmu-gate".to_owned());
        arguments.push(value.to_string());
    }
    if args.signal_drop_vbusvld {
        arguments.push("--usb-ep0-signal-drop-vbus".to_owned());
    }
    if let Some(secs) = args.connect_delay {
        arguments.push("--usb-connect-delay".to_owned());
        arguments.push(secs.to_string());
    }
    if args.smmu_install_bypass {
        arguments.push("--usb-ep0-smmu-install".to_owned());
    }
    if args.signal_dma_probe {
        arguments.push("--usb-signal-dma-probe".to_owned());
    }
    if args.smmu_install_all {
        arguments.push("--usb-smmu-install-all".to_owned());
    }
    if let Some(mode) = args.signal_fsr_gate {
        arguments.push("--usb-signal-fsr-gate".to_owned());
        arguments.push(mode.to_string());
    }
    if args.signal_ram_gate {
        arguments.push("--usb-signal-ram-gate".to_owned());
    }
    if args.skip_typec_spmi {
        arguments.push("--usb-skip-typec-spmi".to_owned());
    }
    if args.signal_diag_publish {
        arguments.push("--usb-signal-diag-publish".to_owned());
    }
    if let Some(secs) = args.quiet_after {
        arguments.push("--usb-quiet-after".to_owned());
        arguments.push(secs.to_string());
    }
    if let Some(secs) = args.observe_secs {
        arguments.push("--usb-observe-secs".to_owned());
        arguments.push(secs.to_string());
    }
    if let Some(origin) = &args.dma_origin {
        arguments.push("--usb-dma-origin".to_owned());
        arguments.push(origin.clone());
    }
    if let Some(value) = &args.signal_cmd_gate {
        arguments.push("--usb-signal-cmd-gate".to_owned());
        arguments.push(value.clone());
    }
    if let Some(value) = &args.signal_rsc_gate {
        arguments.push("--usb-signal-rsc-gate".to_owned());
        arguments.push(value.clone());
    }
    if let Some(value) = &args.signal_cfg_gate {
        arguments.push("--usb-signal-cfg-gate".to_owned());
        arguments.push(value.clone());
    }
    if let Some(value) = args.signal_ramclk_gate {
        arguments.push("--usb-signal-ramclk-gate".to_owned());
        arguments.push(value.to_string());
    }
    if args.smmu_disable {
        arguments.push("--usb-smmu-disable".to_owned());
    }
    if let Some(mode) = args.signal_evt_data_gate {
        arguments.push("--usb-signal-evt-data-gate".to_owned());
        arguments.push(mode.to_string());
    }
    if args.start_after_reset {
        arguments.push("--usb-gadget-handoff-start-after-reset".to_owned());
    }
    if args.start_at_connect_done {
        arguments.push("--usb-gadget-handoff-start-at-connect-done".to_owned());
    }
    if let Some(stage) = args.stop_after_stage {
        arguments.push("--stop-after-stage".to_owned());
        arguments.push(stage.to_string());
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

/// Return the phone to the only state from which the next matrix child may
/// run. This is deliberately an ADB reboot request, never a flash/erase
/// operation; it covers the normal probe failure path where the kernel's
/// watchdog has already handed control to stock Android.
fn restore_fastboot(serial: &str, timeout_secs: u64) -> io::Result<()> {
    if wait_for_fastboot(serial, timeout_secs.min(3)).is_ok() {
        return Ok(());
    }
    let output = Command::new("adb")
        .args(["-s", serial, "reboot", "bootloader"])
        .output()?;
    if !output.status.success() {
        // Android can disappear from ADB in the same instant that it is
        // already transitioning to the bootloader. Treat the transient
        // "device not found" result as a race and give Fastboot the rest of
        // the bounded recovery window to appear. This keeps the Rust loop
        // unattended without retrying a reboot command against a changing
        // USB identity.
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if !stderr.contains("not found") && !stderr.contains("no devices") {
            return Err(io::Error::other(format!(
                "adb reboot bootloader failed: {stderr}"
            )));
        }
        if wait_for_fastboot(serial, timeout_secs).is_ok() {
            return Ok(());
        }
        return Err(io::Error::other(format!(
            "adb reboot bootloader raced with USB disappearance: {stderr}"
        )));
    }
    wait_for_fastboot(serial, timeout_secs)
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

fn create_run_dir(workspace: &Path, prefix: &str) -> io::Result<PathBuf> {
    let base = workspace.join("tmp");
    fs::create_dir_all(&base)?;
    for suffix in 0..1000u32 {
        let path = base.join(format!("{prefix}.{}.{}", std::process::id(), suffix));
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

#[cfg(test)]
mod tests {
    use super::{TRACE_HEADER_BYTES, TRACE_MAGIC, TRACE_VERSION, parse_trace_header};

    #[test]
    fn trace_header_is_little_endian_and_bounded() {
        let mut response = vec![0; TRACE_HEADER_BYTES];
        response[0..4].copy_from_slice(&TRACE_MAGIC.to_le_bytes());
        response[4..8].copy_from_slice(&TRACE_VERSION.to_le_bytes());
        response[8..12].copy_from_slice(&37u32.to_le_bytes());
        response[12..16].copy_from_slice(&37u32.to_le_bytes());
        let header = parse_trace_header(&response).unwrap();
        assert_eq!(header.head, 37);
        assert_eq!(header.valid, 37);
    }

    #[test]
    fn trace_header_rejects_invalid_magic_and_count() {
        let mut response = vec![0; TRACE_HEADER_BYTES];
        response[0..4].copy_from_slice(&0u32.to_le_bytes());
        response[4..8].copy_from_slice(&TRACE_VERSION.to_le_bytes());
        response[12..16].copy_from_slice(&257u32.to_le_bytes());
        assert!(parse_trace_header(&response).is_err());
    }
}
