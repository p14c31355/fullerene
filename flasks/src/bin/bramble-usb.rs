//! Rust replacement for the Bramble USB handoff shell harness.
//!
//! The harness deliberately delegates image construction and the actual
//! Fastboot protocol to Flasks, so the image-operation safety boundary stays
//! in one place: the only device-side image operation is `fastboot boot`.
//! The selected handset transitions from Android ADB into its bootloader by
//! default; `--no-adb-reboot-to-fastboot` is the explicit passive override.

use clap::{Parser, Subcommand, ValueEnum};
use nusb::transfer::{ControlIn, ControlType, Recipient};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use tokio::runtime::Builder;

const DEFAULT_SERIAL: &str = "26191JECB00076";
const DEFAULT_TEMPLATE: &str = "tmp/bramble-stock-boot.img";
const BOOTLOADER_USB: &str = "18d1:4ee0";
const ANDROID_FALLBACK_USB: &str = "18d1:4ee7";
const FULLERENE_USB: &str = "1234:0001";
// Gate runs read the gate bit from the handset's return timing: a false gate
// parks for 90 s before resetting, so the recovery wait must cover the park
// plus the Android boot (well beyond 75 s).
const RECOVERY_TIMEOUT_SECS: u64 = 150;
const MAX_CANDIDATE_RECOVERY_WAIT_SECS: u64 = 900;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeviceState {
    AndroidAdbAvailable,
    FastbootAvailable,
    FullereneUsbAvailable,
    UnknownUsbState,
    DeviceAbsent,
    GoogleLogoSuspected,
}

impl DeviceState {
    fn as_str(self) -> &'static str {
        match self {
            Self::AndroidAdbAvailable => "android-adb-available",
            Self::FastbootAvailable => "fastboot-available",
            Self::FullereneUsbAvailable => "fullerene-usb-1234:0001-available",
            Self::UnknownUsbState => "unknown-usb-state",
            Self::DeviceAbsent => "device-absent",
            Self::GoogleLogoSuspected => "google-logo-or-software-unrecoverable-suspected",
        }
    }
}

#[derive(Debug)]
struct HostObservation {
    state: DeviceState,
    adb_state: Option<String>,
    adb_devices: String,
    fastboot_devices: String,
    lsusb: String,
    fastboot: bool,
    fullerene_usb: bool,
    bootloader_usb: bool,
    android_usb: bool,
}

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
    /// Try the tracked normal Android-init DMA-cache candidates in sequence.
    Candidates(CandidatesArgs),
    /// Read the current host-visible Pixel transport state without changing it.
    Status(StatusArgs),
    /// Read the retained post-mortem USB trace from an enumerated Fullerene gadget.
    Trace(TraceArgs),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Route {
    /// Give the DWC3 device-event SPI to the probe's IRQ consumer instead of
    /// the direct handoff's polling loop.
    Controller,
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
            Self::Controller => "controller",
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
    /// Allow the known direct USB2 handoff's bounded pre-attach work and the
    /// first host descriptor attempt to complete before declaring no attach.
    #[arg(long, default_value_t = 60)]
    enum_timeout: u64,
    #[arg(long, default_value_t = 30)]
    hold: u64,
    #[arg(long, default_value_t = 30)]
    fastboot_wait: u64,
    /// Capture the host's passive usbmon stream for the entire RAM-only boot
    /// attempt. This is host observation only; it does not alter the target
    /// image or USB traffic. usbmon0 records all USB buses, which avoids
    /// assuming that the handset keeps the same Linux bus number.
    #[arg(long)]
    usbmon: bool,
    /// Explicitly allow the selected ADB device to transition to Fastboot.
    /// The safe ADB-to-Fastboot transition is enabled by default; use
    /// --no-adb-reboot-to-fastboot for a passive Fastboot-only run.
    #[arg(long)]
    adb_reboot_to_fastboot: bool,
    /// Do not issue the safe ADB-to-Fastboot transition when Android ADB is
    /// the initial state; wait for Fastboot instead.
    #[arg(long, conflicts_with = "adb_reboot_to_fastboot")]
    no_adb_reboot_to_fastboot: bool,
    #[arg(long)]
    irq_route: Option<Route>,
    #[arg(long)]
    super_speed: bool,
    /// Force QMP's USB lane A or B without changing PMIC Type-C role state.
    #[arg(long, value_parser = ["a", "b"])]
    qmp_lane: Option<String>,
    /// Use the exact same-build Factory XBL `xbl_config` SuperSpeed PHY table
    /// instead of the normal DT/Linux-derived table.
    #[arg(long)]
    xbl_qmp_table: bool,
    /// Add the exact same-build Factory XBL fourth HS-PHY override
    /// (`0x78 <- 0x03`) after the three stock DT override pairs.
    #[arg(long)]
    xbl_hs_phy_table: bool,
    /// Stop immediately after a QMP phase marker (1..=8) and use same-boot
    /// USB2 attach presence as the reached/not-reached readout.
    #[arg(long, value_name = "PHASE", value_parser = clap::value_parser!(u32).range(1..=8))]
    qmp_phase_stop: Option<u32>,
    #[arg(long)]
    normal: bool,
    /// Build the normal AArch64 kernel with Rust `/init` while retaining the
    /// selected USB probe/handoff flags. This keeps Android userspace out of
    /// the USB enumeration experiment.
    #[arg(long)]
    android_init: bool,
    /// Keep the Android-init image's Bramble UFS probe enabled. By default
    /// the harness passes an explicit `0`, preserving the USB-only safety
    /// boundary and making the effective storage policy auditable.
    #[arg(long, requires = "android_init")]
    android_init_ufs_execute: bool,
    /// Run the normal AArch64 USB handoff before MMU/allocator setup, matching
    /// the source-backed early boundary used by the Android-init candidate.
    #[arg(long, requires = "android_init")]
    early_usb_handoff: bool,
    /// Run that handoff before the normal path scans the boot DTB/resources.
    /// This is a stricter ordering A/B for separating DT discovery from USB.
    #[arg(long, requires = "android_init")]
    early_usb_before_dtb_scan: bool,
    /// Apply the entry secure-watchdog ownership boundary before DTB walking.
    #[arg(long, requires = "android_init")]
    entry_secure_wdt: bool,
    /// Enable the build-gated standard ADB return path for this Android-init
    /// verification image.
    #[arg(long, requires = "android_init")]
    adb_return: bool,
    /// Run the normal non-destructive handoff first, with the probe's
    /// retained-trace watchdog and automatic recovery still enabled.
    #[arg(long)]
    direct_handoff: bool,
    #[arg(long)]
    pullup_only: bool,
    /// Run the minimal USB2 pull-up sequence without DWC3 reset, DMA, or EP0.
    #[arg(long)]
    bare_pullup: bool,
    /// Bare-pullup bisection: stop the bare handoff after checkpoint K
    /// (1 = PHY/session votes + USB2 PHY wake, 2 = +UTMI clock mux,
    /// 3 = +GCTL/DCFG/DALEPENA, 4 = full through Run/Stop start) and park.
    /// The host attach time then measures the cumulative cost of the
    /// executed prefix, separating ABL-to-MMIO latency from per-step cost.
    #[arg(long = "bare-pullup-stop-after", value_name = "K")]
    bare_pullup_stop_after: Option<u32>,
    /// Bare-pullup bisection: fire the pull-up sequence at the very
    /// first instruction after EL1 entry (before relocation and the
    /// prelude), then spin. Attach time measures the ABL/XBL-to-
    /// kernel-entry latency alone; an unchanged T+10-11 attach means
    /// the pre-attach gap is on the bootloader side.
    #[arg(long)]
    hyper_bare: bool,
    /// Publish only the physical pull-up after one gadget handoff boundary
    /// (1..=29; stage 13 is the QMP-complete SS boundary, stage 14 is the
    /// post-global-control SS boundary, and stages 15..=20 bisect the
    /// post-stage-14 tail), then use the
    /// normal automatic recovery path.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=29))]
    stop_after_stage: Option<u32>,
    #[arg(long)]
    no_smmu: bool,
    /// Force cache clean/invalidate operations for the standalone probe's
    /// DMA window, which normally uses the no-MMU uncached fast path (A/B).
    #[arg(long)]
    dma_cache_maintenance: bool,
    /// Reuse Fastboot's event-ring DMA page instead of the linker-reserved
    /// EP0 DMA objects. This is a Rust-only firmware-mapping differential.
    #[arg(long)]
    reuse_fastboot_dma: bool,
    #[arg(long)]
    no_transfer_resource: bool,
    #[arg(long)]
    android_resource_order: bool,
    /// Replay Linux's non-endpoint gadget-start defaults immediately before
    /// the final Run/Stop boundary (single source-directed ordering A/B).
    #[arg(long)]
    gadget_start_defaults_at_runstop: bool,
    /// Enforce qpr1's 50 ms minimum stop-to-start interval before the final
    /// direct USB2 gadget Run/Stop transition (A/B).
    #[arg(long)]
    min_runstop_delay: bool,
    /// Re-enable Android msm's iface/core/sleep controller branches before
    /// the UTMI branch at the direct USB2 handoff boundary.
    #[arg(long)]
    clock_branches_rearm: bool,
    /// Select Android msm's HS performance state for the DWC3 core clock
    /// (66.666667 MHz) at the direct USB2 handoff boundary.
    #[arg(long)]
    usb_core_hs_clock: bool,
    /// Use a broader Fullerene DWC3 controller-domain reset A/B on the
    /// direct USB2 handoff (DCTL CSFTRST, then GCTL CORESOFTRESET plus the
    /// USB2 PHY-facing soft-reset). Combine with --hsphy-before-reset only
    /// when comparing the external HS-PHY ordering.
    #[arg(long)]
    usb2_full_core_reset: bool,
    /// Wait after the controller clock branches are enabled, in microseconds
    /// (0..=20000), before the first DWC3 setup write.
    #[arg(long, value_parser = clap::value_parser!(u32).range(0..=20_000))]
    clock_stable_delay_us: Option<u32>,
    /// Reproduce Android msm's controller block-reset clock boundary.
    #[arg(long)]
    android_block_reset: bool,
    /// Re-assert Android msm's three QUSB2 HS-PHY regulator rails before
    /// the direct handoff reset/init boundary (A/B).
    #[arg(long)]
    refresh_hsphy_power: bool,
    /// Program the Android msm_hsphy_init() vdda18/vdda33 voltage ranges
    /// before enabling the refreshed HS-PHY rails (A/B).
    #[arg(long)]
    hsphy_program_vdda_voltage: bool,
    /// Send HS-PHY regulator requests to both qcom,set=3 TCS families,
    /// matching the Android RPMh regulator contract (A/B).
    #[arg(long)]
    hsphy_all_regulator_sets: bool,
    /// Skip the direct handoff's explicit QUSB2 PHY block-reset pulse (A/B).
    #[arg(long)]
    skip_usb2_phy_reset: bool,
    /// Use Android msm's 4096-byte control event buffer instead of the
    /// XBL-derived 0xf0-byte event ring (Bramble A/B).
    #[arg(long)]
    event_ring_size_4096: bool,
    /// Arm EP0 STARTTRANSFER immediately after Run/Stop (Bramble A/B).
    #[arg(long)]
    start_after_connect: bool,
    /// Historical XBL differential for EP0 request ownership. It is not the
    /// source-confirmed initial SETUP arm model; use only for reproduction.
    #[arg(long)]
    xbl_deferred_setup: bool,
    /// Use XBL's NORMAL TRBCTL=1 for EP0 IN data responses only.
    #[arg(long)]
    xbl_ep0_in_data: bool,
    /// Place only the EP0 event ring at XBL's observed 0x0a6fc010 address.
    #[arg(long)]
    xbl_event_dma: bool,
    /// Match stock XBL's EP0 SETEPCONFIG notification mask (P1=0x300).
    #[arg(long)]
    xbl_ep0_config: bool,
    /// Mirror XBL's initial EP0 request insertion between OUT and IN setup.
    #[arg(long)]
    xbl_between_ep0: bool,
    /// Apply XBL's usb31 global deltas after the EP0 endpoint pairs (A/B).
    #[arg(long)]
    xbl_post_endpoint_global: bool,
    /// Use XBL's fixed initial EP0 setup buffer and TRB addresses (A/B).
    #[arg(long)]
    xbl_stock_ep0_dma: bool,
    /// Change only DCTL.RUN_STOP at the final XBL handoff boundary (A/B).
    #[arg(long)]
    xbl_raw_runstop: bool,
    /// Apply only the source-exact DCTL Run/Stop bit change on the SS path.
    #[arg(long)]
    source_exact_runstop: bool,
    /// Reassert DCTL.RUN_STOP if the SS start transition clears it.
    #[arg(long)]
    ss_reassert_runstop: bool,
    /// Keep reasserting DCTL.RUN_STOP during the SS link-training window.
    #[arg(long)]
    ss_hold_runstop: bool,
    /// Retry SS EP0 STARTTRANSFER after revoking a stale transfer resource.
    #[arg(long)]
    ss_retry_setup: bool,
    /// Arm the SS EP0 SETUP transfer before Run/Stop, matching qpr1's
    /// __dwc3_gadget_start() ordering (A/B).
    #[arg(long)]
    ss_eager_setup: bool,
    /// Keep USB3 PIPE SUSPHY asserted through SS endpoint construction, as
    /// qpr1's dwc3_phy_setup() does before gadget Run/Stop (A/B).
    #[arg(long)]
    ss_source_susphy: bool,
    /// Clear DCTL.HIRD_THRES at SuperSpeed Connect Done, matching qpr1's
    /// dwc3_gadget_conndone_interrupt() non-HS branch (A/B).
    #[arg(long)]
    ss_conndone_clear_hird: bool,
    /// Use Bramble's DT HIRD threshold (0x10) instead of XBL's observed 7.
    #[arg(long)]
    dt_hird_threshold: bool,
    /// Apply Android msm's HS Connect Done LPM/HIRD controller policy.
    #[arg(long)]
    android_hs_lpm: bool,
    /// Extend the HS Connect Done policy with the DT snps,has-lpm-erratum
    /// field (DCTL.LPM_ERRATA=0xf) exactly where gadget.c sets it.
    #[arg(long, requires = "android_hs_lpm")]
    android_lpm_errata: bool,
    /// Mirror Factory ABL's additional QUSB2 HS PHY ATE/test cleanup (A/B).
    #[arg(long)]
    abl_shared_hs_phy: bool,
    /// Use Factory ABL's observed narrow DWC3 device-event mask (0x47).
    #[arg(long)]
    abl_devten: bool,
    /// Match Factory ABL/Qualcomm msm's EP0 SETEPCONFIG fields (A/B).
    #[arg(long)]
    abl_ep_config: bool,
    /// Use Factory ABL's command-kind parameter-write mask (A/B).
    #[arg(long)]
    abl_command_params: bool,
    /// Use Factory ABL's EP0 request TRB flags HWO|CHN|ISP_IMI (0x405) (A/B).
    #[arg(long)]
    abl_trb_flags: bool,
    /// Use Factory ABL's CONTROL_SETUP buffer pointer: the EP0 TRB address
    /// itself rather than the separate 8-byte setup buffer (A/B).
    #[arg(long)]
    abl_setup_trb_buffer: bool,
    /// Consume each EP0 event after dispatching it, matching Factory ABL's
    /// four-byte GEVNTCOUNT acknowledgement order (A/B).
    #[arg(long)]
    abl_event_consume: bool,
    /// Use XBL's separate EP0 OUT/IN TRB slots for direction-specific transfers (A/B).
    #[arg(long)]
    xbl_direction_trb: bool,
    /// Add XBL's chained-transfer bit to EP0 TRBs (A/B).
    #[arg(long)]
    xbl_trb_chain: bool,
    /// Retry EP0 STARTTRANSFER after Run/Stop without reading DSTS.USBLNKST.
    #[arg(long)]
    start_ungated: bool,
    /// Re-publish the EP0 event buffer immediately before Run/Stop.
    #[arg(long)]
    event_ring_at_runstop: bool,
    /// Re-run the Android msm gadget-start EP0 sequence immediately before
    /// Run/Stop.
    #[arg(long)]
    gadget_restart_at_runstop: bool,
    /// Skip the initial EP0 construction and perform the Android gadget-start
    /// sequence only at the final Run/Stop boundary (source-order A/B).
    #[arg(long)]
    gadget_start_only_at_runstop: bool,
    /// Reproduce Qualcomm's DWC3_CONTROLLER_NOTIFY_CLEAR_DB immediately
    /// after the device-core reset (A/B).
    #[arg(long)]
    clear_gsi_after_reset: bool,
    /// Use the source-exact Bramble msm_hsphy_init() sequence on the direct
    /// USB2 handoff instead of the legacy helper's local RTUNE/delay steps.
    #[arg(long)]
    hsphy_source_exact: bool,
    /// Use the exact same-build XBL usb_shared_hs_phy_init() sequence on the
    /// direct USB2 handoff: XBL's four tuning pairs and cleanup ordering,
    /// without qpr1-only VBUS override writes.
    #[arg(long)]
    hsphy_xbl_exact: bool,
    /// Force the historical Bramble HS-PHY tuning pairs 0x63/0x85 for a
    /// physical control run; the qpr1 source-confirmed pairs remain default.
    #[arg(long)]
    hsphy_legacy_fallback: bool,
    /// Run the HS-PHY reset/init before the DWC3 device-core reset, matching
    /// qpr1's msm_usb2_phy_probe() ownership order (A/B).
    #[arg(long)]
    hsphy_before_reset: bool,
    /// Restore the qpr1 HS-PHY SUSPEND_N bit immediately after Run/Stop
    /// (one-variable physical A/B; direct handoff only).
    #[arg(long)]
    hsphy_restore_suspend_n_after_runstop: bool,
    /// Re-run qpr1's selected SUSPEND_N write sequence immediately after
    /// Run/Stop: set SUSPEND_N_SEL|SUSPEND_N, then clear SUSPEND_N_SEL.
    #[arg(long)]
    hsphy_restore_suspend_n_selected_after_runstop: bool,
    /// Start EP0 with the Linux/Android 512-byte descriptor state.
    #[arg(long)]
    ep0_initial_512: bool,
    /// Keep DCFG at the Bramble maximum-speed SuperSpeed state at Run/Stop.
    #[arg(long)]
    dcfg_superspeed: bool,
    /// Force the direct USB2 handoff to DWC3.DCFG.FULLSPEED (A/B).
    #[arg(long)]
    dcfg_fullspeed: bool,
    /// Force the direct USB2 handoff to DWC3.DCFG.LOWSPEED (A/B).
    #[arg(long)]
    dcfg_lowspeed: bool,
    /// Omit qpr1's SuperSpeed lane power-present VBUS override on the
    /// High-Speed-only direct USB2 handoff (A/B).
    #[arg(long)]
    no_ss_vbus: bool,
    /// Repeat qpr1's device-core soft reset immediately before the USB2
    /// Run/Stop boundary, then rebuild EP0 state (A/B).
    #[arg(long)]
    usb2_core_reset_at_runstop: bool,
    /// Match qpr1's device-core soft reset exactly: raw DCTL.CSFTRST,
    /// 1-ms polling, and the post-reset doorbell clear (A/B).
    #[arg(long)]
    usb2_source_exact_device_reset: bool,
    /// Keep qpr1's short post-reset UTMI/Pipe mux turn and omit the
    /// historical standalone 100-us clock-source transition (A/B).
    #[arg(long)]
    usb2_qpr1_utmi_post_reset_only: bool,
    /// Do not write USB2 PHYIF/TRDTIM, matching qpr1's UNKNOWN DT mode (A/B).
    #[arg(long)]
    usb2_preserve_phy_interface: bool,
    /// Re-assert DWC3 GCTL device mode immediately before SS Run/Stop.
    #[arg(long)]
    ss_reassert_device_mode: bool,
    /// Re-assert the USB30 GDSC and DWC3 controller clocks after QMP init.
    #[arg(long)]
    ss_reassert_core_clocks: bool,
    /// Re-assert the USB30 GDSC and DWC3 controller clocks immediately after
    /// the SuperSpeed Run/Stop transition (A/B).
    #[arg(long)]
    ss_reassert_core_clocks_after_runstop: bool,
    /// Re-send Android-style USB domain votes/rails, then re-assert the USB30
    /// controller domain immediately after Run/Stop (A/B).
    #[arg(long)]
    ss_reassert_domain_after_runstop: bool,
    /// Replay Qualcomm msm's link-clock stop/core-reset/release sequence
    /// immediately after SS Run/Stop (A/B).
    #[arg(long)]
    ss_reassert_link_clocks_after_runstop: bool,
    /// Replay Android msm's DBM soft-reset/enable sequence before SS
    /// endpoint publication (the `core_reset = false` block-reset path).
    #[arg(long)]
    ss_android_dbm_reset: bool,
    /// Re-assert QMP common/PCS power-up after QMP init (A/B).
    #[arg(long)]
    ss_reassert_qmp_power: bool,
    /// Re-assert QMP common/PCS power-up after DWC3 global-control setup,
    /// matching the USB3 PHY resume ordering (A/B).
    #[arg(long)]
    ss_reassert_qmp_power_after_gctl: bool,
    /// Re-run the official USB2 legacy-PHY power/reset/init sequence before
    /// the no-core SuperSpeed QMP reset/init boundary (A/B).
    #[arg(long)]
    ss_reinit_hs_phy: bool,
    /// Apply the controller-side dwc3_phy_setup() writes before the
    /// no-core SuperSpeed QMP reset/init boundary (A/B).
    #[arg(long)]
    ss_pre_qmp_phy_setup: bool,
    /// Clear QMP autonomous mode after the USB3 PHY resume boundary (A/B).
    #[arg(long)]
    ss_clear_qmp_autonomous: bool,
    /// Re-assert QMP aux/pipe/com_aux clock branches after QMP init (A/B).
    #[arg(long)]
    ss_reassert_qmp_clocks: bool,
    /// Re-assert QMP aux/pipe/com_aux clock branches after DWC3 global
    /// control setup, matching the USB3 PHY resume ordering (A/B).
    #[arg(long)]
    ss_reassert_qmp_clocks_after_gctl: bool,
    /// Re-assert the Bramble USB2 PHY ref_clk_src after DWC3 global control
    /// setup, matching usb_phy_set_suspend(usb2, 0) ordering (A/B).
    #[arg(long)]
    ss_reassert_hs_phy_ref_after_gctl: bool,
    /// Clear the Qualcomm DWC3 sleep-mode bits before the SuperSpeed gadget
    /// start, matching dwc3_otg_start_peripheral() (A/B).
    #[arg(long)]
    ss_dis_sleep_mode_before_gadget: bool,
    /// Write literal zero to the QMP autonomous-mode register, matching the
    /// official connected-cable resume path (A/B).
    #[arg(long)]
    ss_clear_qmp_autonomous_exact: bool,
    /// Apply the official arm64 wmb() after QMP resume writes (A/B).
    #[arg(long)]
    ss_qmp_resume_wmb: bool,
    /// Use the official arm64 wmb() between the QMP LFPS IRQ-clear writes
    /// (A/B).
    #[arg(long)]
    ss_qmp_lfps_clear_wmb: bool,
    /// Replay the official QMP USB PHY disconnect-notifier power-down write
    /// before the no-core QMP reset/init (A/B).
    #[arg(long)]
    ss_qmp_notify_disconnect: bool,
    /// Clear the official Qualcomm USB2/USB3 VBUS/session overrides before
    /// the no-core QMP reset/init (A/B).
    #[arg(long)]
    ss_clear_vbus_override_before_qmp: bool,
    /// Clear DCTL.KEEP_CONNECT on the old-session stop when hibernation is
    /// supported, matching dwc3_gadget_run_stop(..., false, false) (A/B).
    #[arg(long)]
    ss_clear_keep_connect_before_stop: bool,
    /// Clear USB3 GUSB3PIPECTL.SUSPHY after old-session teardown, matching
    /// dwc3_usb3_phy_suspend(dwc, false) (A/B).
    #[arg(long)]
    ss_clear_usb3_susphy_before_qmp: bool,
    /// Clear USB3 GUSB3PIPECTL.SUSPHY immediately before final Run/Stop
    /// (diagnostic A/B for a SuperSpeed link that has no EP0 response).
    #[arg(long)]
    ss_clear_usb3_susphy_before_runstop: bool,
    /// Clear USB3 GUSB3PIPECTL.SUSPHY immediately after final Run/Stop and
    /// retain the readback in the DWC3 boundary marker (diagnostic A/B).
    #[arg(long)]
    ss_clear_usb3_susphy_after_runstop: bool,
    /// Repeat Android's DWC3 device-core reset immediately before final
    /// SuperSpeed Run/Stop, then rebuild the EP0 start state (diagnostic A/B).
    #[arg(long)]
    ss_core_reset_at_runstop: bool,
    /// Use Android's separate eight-byte EP0 SETUP buffer rather than
    /// aliasing the setup packet to the EP0 TRB ring entry (USB2/SS A/B).
    #[arg(long)]
    ss_separate_setup_buffer: bool,
    /// Disable DWC3 gadget event interrupts before old-session stop, matching
    /// dwc3_gadget_disable_irq() in the official teardown (A/B).
    #[arg(long)]
    ss_disable_gadget_irq_before_stop: bool,
    /// Disable EP0 OUT/IN in DALEPENA before old-session stop, matching the
    /// official dwc3_gadget_run_stop(false) endpoint teardown (A/B).
    #[arg(long)]
    ss_disable_ep0_before_stop: bool,
    /// Clear the official GSI event-buffer and Qualcomm doorbell state after
    /// old-session DCTL.Run/Stop is cleared (A/B).
    #[arg(long)]
    ss_clear_gsi_stop_state: bool,
    /// Apply Qualcomm msm's USB31 LFPS exit-response timer values immediately
    /// before the SuperSpeed gadget start (A/B).
    #[arg(long)]
    ss_lfps_timer: bool,
    /// Clear DWC31 GUSB3PIPECTL.UX_EXIT_PX as in dwc3_phy_setup() (A/B).
    #[arg(long)]
    ss_clear_ux_exit_px: bool,
    /// Preserve the DWC3 reference-clock timing registers instead of applying
    /// the historical non-Bramble calibration (A/B).
    #[arg(long)]
    ss_preserve_ref_clock_state: bool,
    /// Preserve Fastboot's already-trained USB3/QMP PHY state and only
    /// rebuild the DWC3 gadget/EP0 state (A/B).
    #[arg(long)]
    ss_preserve_phy_state: bool,
    /// Set DCFG.IGNSTRMPP in the direct gadget-start sequence (A/B).
    #[arg(long)]
    dcfg_ignstrmpp: bool,
    /// Restore USB2 SUSPHY immediately before the direct Run/Stop boundary.
    #[arg(long)]
    usb2_susphy: bool,
    /// Restore USB2 SUSPHY immediately after the SuperSpeed Run/Stop boundary
    /// (A/B; this is intentionally separate from the direct USB2 option).
    #[arg(long)]
    usb2_susphy_after_runstop: bool,
    /// Keep USB2 SUSPHY enabled through qpr1's endpoint/resource setup;
    /// endpoint commands still clear it transiently as Linux does (A/B).
    #[arg(long)]
    usb2_source_susphy: bool,
    /// Use qpr1's exact DWC3 device-event enable mask, including vendor,
    /// overflow, command-complete, and erratic-error events (A/B).
    #[arg(long)]
    usb2_source_exact_devten: bool,
    /// Publish the qpr1 device-event mask before Run/Stop when EP0
    /// STARTTRANSFER is deferred (A/B).
    #[arg(long)]
    usb2_source_devten_before_runstop: bool,
    /// Force qpr1's USB2 SUSPHY/ENBLSLPM guard around every EP command;
    /// this avoids trusting a stale Fastboot DSTS speed value (A/B).
    #[arg(long)]
    usb2_source_exact_cmd_guard: bool,
    /// Apply qpr1's USB2 gadget Run/Stop write: keep the current DCTL policy
    /// and change only the source-required RUN_STOP bit (A/B).
    #[arg(long)]
    usb2_source_exact_runstop: bool,
    /// Apply qpr1 dwc3_phy_setup()'s USB3 PIPE setup before the USB2 core
    /// reset: clear UX_EXIT_PX and assert USB3 SUSPHY (A/B).
    #[arg(long)]
    usb2_source_phy_setup: bool,
    /// Clear DWC3 USB2 sleep-mode bits before the direct gadget handoff,
    /// matching qpr1 dwc3_dis_sleep_mode() (A/B).
    #[arg(long)]
    usb2_dis_sleep_mode: bool,
    /// Replay qpr1 dwc3_msm_block_reset(false): reset and enable the
    /// Qualcomm DBM before the direct USB2 gadget start (A/B).
    #[arg(long)]
    usb2_android_dbm_reset: bool,
    /// Issue the Linux dwc3_ep0_stall_and_restart() EP0 SETSTALL flush and
    /// arm the SETUP TRB at the halted pre-Run/Stop boundary (A/B).
    #[arg(long)]
    ep0_stall_flush: bool,

    /// Cap the first GET_DESCRIPTOR(device) data-phase response at 8 bytes
    /// (short-packet tolerance probe for the EP0 IN data path).
    #[arg(long)]
    ep0_short_first_desc: bool,

    /// Raise the EP0 IN TX FIFO (GTXFIFOSIZ(0)) to a safe depth when the
    /// handoff left it degenerate.
    #[arg(long)]
    ep0_txfifo_fix: bool,
    /// Clear GUSB2PHYCFG.U2_FREECLK_EXISTS after controller reset (A/B).
    #[arg(long)]
    u2_freeclk_clear: bool,
    /// Set GUSB2PHYCFG.U2_FREECLK_EXISTS after controller reset (A/B).
    #[arg(long)]
    u2_freeclk_set: bool,
    /// Arm the initial EP0 SETUP only after the host USB Reset event.
    #[arg(long)]
    start_after_reset: bool,
    /// Arm the initial EP0 SETUP from the DWC3 Connect Done event.
    #[arg(long)]
    start_at_connect_done: bool,
    /// Reallocate both EP0 transfer resources after the host USB Reset.
    #[arg(long)]
    reset_resource: bool,
    /// Rebuild both EP0 endpoint contexts after the host USB Reset.
    #[arg(long)]
    reset_endpoints: bool,
    /// Clear EP0 OUT/IN stall state after the host USB Reset while preserving
    /// the armed SETUP transfer, matching Android msm's reset handler.
    #[arg(long)]
    ep0_reset_clear_stall: bool,
    /// Clear DCTL.TSTCTRL after the host USB Reset, matching Android msm's
    /// reset handler without changing the preserved EP0 transfer.
    #[arg(long)]
    ep0_reset_clear_test_mode: bool,
    /// Invoke the gadget reset callback before controller cleanup after the
    /// host USB Reset, matching Android msm's ordering.
    #[arg(long)]
    ep0_reset_callback_first: bool,
    /// Apply Android msm's reset callback, test-mode clear, active-transfer
    /// revoke, and EP0 stall clear in qpr1 source order, then re-arm EP0.
    #[arg(long)]
    ep0_reset_android_state_order: bool,
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
    /// Probe event-DMA liveness after Run/Stop and after the link reaches U0.
    #[arg(long)]
    signal_dma_post_runstop: bool,
    /// Install the SMR as a catch-all (mask all IDs) instead of exact 0xe0.
    #[arg(long)]
    smmu_install_all: bool,
    /// FSR gate: 1 = attach only when the SMMU faulted during the probe.
    #[arg(long = "signal-fsr-gate", value_name = "MODE")]
    signal_fsr_gate: Option<u32>,
    /// Previous-boot trace gate: 1 = previous trace reached a SETUP, 2 = it
    /// did not, 3 = valid trace with no SETUP, 4 = EP0 SETUP transfer was
    /// armed but no SETUP arrived, 5 = Connect Done arrived but no SETUP.
    /// A suppressed run resets without publishing the pull-up, so the
    /// kernel.log attach-line presence is the one-bit readout.
    #[arg(long = "signal-prev-trace-gate", value_name = "MODE")]
    signal_prev_trace_gate: Option<u32>,
    /// Previous-boot QMP phase gate: 1=entry, 2=preamble, 3=table,
    /// 4=table complete, 5=PCS start, 6=status read, 7=poll, 8=PHY ready.
    #[arg(long = "signal-prev-qmp-gate", value_name = "PHASE")]
    signal_prev_qmp_gate: Option<u32>,
    /// Gate the attach on a CPU readback of the .usb_dma region succeeding.
    #[arg(long)]
    signal_ram_gate: bool,
    /// Skip the SPMI Type-C handoff observation at probe entry (timing A/B).
    #[arg(long)]
    skip_typec_spmi: bool,
    /// After a failed handoff, re-issue the missing init tail (Run/Stop, U0
    /// poll, DEPSTARTCFG, SETEPCONFIG, SETUP arm) from the signal probe.
    #[arg(long)]
    u0_arm_probe: bool,
    /// Stop a still-running controller before the U0 arm probe rebuilds the
    /// DWC3 endpoint/resource state.
    #[arg(long)]
    u0_arm_stop_first: bool,
    /// Control: unconditional APSS-WDT bite 3 s after probe entry; an early
    /// loop return proves the APSS watchdog bite is writable and lands.
    #[arg(long)]
    wdt_bite_control: bool,
    /// Override the secure-watchdog-disable SMC fnid (hex, e.g.
    /// 0x82000107 = STD/SMC64 BOOT/0x07). A return far past the ~37 s
    /// secure-WDT bucket means the bite was actually disabled.
    #[arg(long = "swdd-fnid", value_name = "HEX")]
    swdd_fnid: Option<String>,
    /// Omit the secure-watchdog-disable SMC itself (timing experiment; the
    /// secure WDT stays armed and bites at ~17 s, harmless to the
    /// attach/-110 readouts).
    #[arg(long)]
    swdd_skip: bool,
    /// Emit one host-visible DWC3 Run/Stop pair after the post-Run/Stop arm
    /// window when the SETUP arm succeeded (a disconnect/re-attach pair at
    /// attach proves the core's link FSM reached U0).
    #[arg(long)]
    arm_blip: bool,
    /// Absolute reset ceiling (seconds) for the direct probe's poll loop:
    /// guarantees a recovery reset even if both watchdogs are dead.
    #[arg(long = "abs-reset-secs", value_name = "SECS")]
    abs_reset_secs: Option<u64>,
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
    /// Publish a read-only live USB2/HS-PHY snapshot field before Run/Stop.
    #[arg(long = "utmi-preconnect-readout", value_name = "SELECTOR")]
    utmi_preconnect_readout: Option<String>,
    /// Publish a read-only live USB2/HS-PHY snapshot field after Run/Stop.
    #[arg(long = "utmi-postrun-readout", value_name = "SELECTOR")]
    utmi_postrun_readout: Option<String>,
    /// Publish one PM8150 PON register through the attach-delay channel:
    /// seq (previous reset-reason bucket, the default), or a raw byte from
    /// wd2 (PMIC-watchdog enable/type), s1/s2 (watchdog timers), ctl, warm,
    /// or soft (reset-reason registers). The byte rides as
    /// (value + 1) * 300 ms capped at 9.6 s.
    #[arg(long = "pon-readout", value_name = "REG")]
    pon_readout: Option<String>,
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
    /// Preserve Fastboot's live DWC3 Run/Stop state while handing the device
    /// to Fullerene, matching the public ABL Stop() path that only frees the
    /// RX/TX buffers.
    #[arg(long)]
    preserve_fastboot_runstop: bool,
    #[arg(long)]
    uncompressed: bool,
    #[arg(long)]
    dry_run: bool,
}

impl Default for LoopArgs {
    fn default() -> Self {
        Self {
            serial: DEFAULT_SERIAL.to_owned(),
            template: PathBuf::from(DEFAULT_TEMPLATE),
            enum_timeout: 60,
            hold: 30,
            fastboot_wait: 30,
            usbmon: false,
            adb_reboot_to_fastboot: false,
            no_adb_reboot_to_fastboot: false,
            irq_route: None,
            super_speed: false,
            qmp_lane: None,
            xbl_qmp_table: false,
            xbl_hs_phy_table: false,
            qmp_phase_stop: None,
            normal: false,
            android_init: false,
            android_init_ufs_execute: false,
            early_usb_handoff: false,
            early_usb_before_dtb_scan: false,
            entry_secure_wdt: false,
            adb_return: false,
            direct_handoff: false,
            pullup_only: false,
            bare_pullup: false,
            bare_pullup_stop_after: None,
            hyper_bare: false,
            stop_after_stage: None,
            no_smmu: false,
            dma_cache_maintenance: false,
            reuse_fastboot_dma: false,
            no_transfer_resource: false,
            android_resource_order: false,
            gadget_start_defaults_at_runstop: false,
            min_runstop_delay: false,
            clock_branches_rearm: false,
            usb_core_hs_clock: false,
            usb2_full_core_reset: false,
            clock_stable_delay_us: None,
            android_block_reset: false,
            refresh_hsphy_power: false,
            hsphy_program_vdda_voltage: false,
            hsphy_all_regulator_sets: false,
            skip_usb2_phy_reset: false,
            event_ring_size_4096: false,
            start_after_connect: false,
            xbl_deferred_setup: false,
            xbl_ep0_in_data: false,
            xbl_event_dma: false,
            xbl_ep0_config: false,
            xbl_between_ep0: false,
            xbl_post_endpoint_global: false,
            xbl_stock_ep0_dma: false,
            xbl_raw_runstop: false,
            source_exact_runstop: false,
            ss_reassert_runstop: false,
            ss_hold_runstop: false,
            ss_retry_setup: false,
            ss_eager_setup: false,
            ss_source_susphy: false,
            ss_conndone_clear_hird: false,
            dt_hird_threshold: false,
            android_hs_lpm: false,
            android_lpm_errata: false,
            abl_shared_hs_phy: false,
            abl_devten: false,
            abl_ep_config: false,
            abl_command_params: false,
            abl_trb_flags: false,
            abl_setup_trb_buffer: false,
            abl_event_consume: false,
            xbl_direction_trb: false,
            xbl_trb_chain: false,
            start_ungated: false,
            event_ring_at_runstop: false,
            gadget_restart_at_runstop: false,
            gadget_start_only_at_runstop: false,
            clear_gsi_after_reset: false,
            hsphy_source_exact: false,
            hsphy_xbl_exact: false,
            hsphy_legacy_fallback: false,
            hsphy_before_reset: false,
            hsphy_restore_suspend_n_after_runstop: false,
            hsphy_restore_suspend_n_selected_after_runstop: false,
            ep0_initial_512: false,
            dcfg_superspeed: false,
            dcfg_fullspeed: false,
            dcfg_lowspeed: false,
            no_ss_vbus: false,
            usb2_core_reset_at_runstop: false,
            usb2_source_exact_device_reset: false,
            usb2_qpr1_utmi_post_reset_only: false,
            usb2_preserve_phy_interface: false,
            ss_reassert_device_mode: false,
            ss_reassert_core_clocks: false,
            ss_reassert_core_clocks_after_runstop: false,
            ss_reassert_domain_after_runstop: false,
            ss_reassert_link_clocks_after_runstop: false,
            ss_android_dbm_reset: false,
            ss_reassert_qmp_power: false,
            ss_reassert_qmp_power_after_gctl: false,
            ss_reinit_hs_phy: false,
            ss_pre_qmp_phy_setup: false,
            ss_clear_qmp_autonomous: false,
            ss_reassert_qmp_clocks: false,
            ss_reassert_qmp_clocks_after_gctl: false,
            ss_reassert_hs_phy_ref_after_gctl: false,
            ss_dis_sleep_mode_before_gadget: false,
            ss_clear_qmp_autonomous_exact: false,
            ss_qmp_resume_wmb: false,
            ss_qmp_lfps_clear_wmb: false,
            ss_qmp_notify_disconnect: false,
            ss_clear_vbus_override_before_qmp: false,
            ss_clear_keep_connect_before_stop: false,
            ss_clear_usb3_susphy_before_qmp: false,
            ss_clear_usb3_susphy_before_runstop: false,
            ss_clear_usb3_susphy_after_runstop: false,
            ss_core_reset_at_runstop: false,
            ss_separate_setup_buffer: false,
            ss_disable_gadget_irq_before_stop: false,
            ss_disable_ep0_before_stop: false,
            ss_clear_gsi_stop_state: false,
            ss_lfps_timer: false,
            ss_clear_ux_exit_px: false,
            ss_preserve_ref_clock_state: false,
            ss_preserve_phy_state: false,
            dcfg_ignstrmpp: false,
            usb2_susphy: false,
            usb2_susphy_after_runstop: false,
            usb2_source_susphy: false,
            usb2_source_exact_devten: false,
            usb2_source_devten_before_runstop: false,
            usb2_source_exact_cmd_guard: false,
            usb2_source_exact_runstop: false,
            usb2_source_phy_setup: false,
            usb2_dis_sleep_mode: false,
            usb2_android_dbm_reset: false,
            ep0_stall_flush: false,
            ep0_short_first_desc: false,
            ep0_txfifo_fix: false,
            u2_freeclk_clear: false,
            u2_freeclk_set: false,
            start_after_reset: false,
            start_at_connect_done: false,
            reset_resource: false,
            reset_endpoints: false,
            ep0_reset_clear_stall: false,
            ep0_reset_clear_test_mode: false,
            ep0_reset_callback_first: false,
            ep0_reset_android_state_order: false,
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
            signal_dma_post_runstop: false,
            smmu_install_all: false,
            signal_fsr_gate: None,
            signal_prev_trace_gate: None,
            signal_prev_qmp_gate: None,
            signal_ram_gate: false,
            skip_typec_spmi: false,
            u0_arm_probe: false,
            u0_arm_stop_first: false,
            wdt_bite_control: false,
            swdd_fnid: None,
            swdd_skip: false,
            arm_blip: false,
            abs_reset_secs: None,
            signal_diag_publish: false,
            quiet_after: None,
            observe_secs: None,
            dma_origin: None,
            signal_cmd_gate: None,
            utmi_preconnect_readout: None,
            utmi_postrun_readout: None,
            pon_readout: None,
            signal_rsc_gate: None,
            signal_cfg_gate: None,
            signal_ramclk_gate: None,
            smmu_disable: false,
            signal_evt_data_gate: None,
            no_core_reset: false,
            preserve_fastboot_runstop: false,
            uncompressed: false,
            dry_run: false,
        }
    }
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
    #[arg(long, default_value_t = 60)]
    enum_timeout: u64,
    #[arg(long, default_value_t = 30)]
    hold: u64,
    #[arg(long, default_value_t = 30)]
    fastboot_wait: u64,
    /// Explicitly allow the selected ADB device to transition to Fastboot.
    /// The safe transition is enabled by default for each matrix route.
    #[arg(long)]
    adb_reboot_to_fastboot: bool,
    /// Keep the matrix passive when Android ADB is the initial state.
    #[arg(long, conflicts_with = "adb_reboot_to_fastboot")]
    no_adb_reboot_to_fastboot: bool,
    #[arg(long)]
    super_speed: bool,
    #[arg(long)]
    no_smmu: bool,
    #[arg(long)]
    no_core_reset: bool,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Parser, Debug, Default)]
struct CandidatesArgs {
    #[arg(long, default_value = DEFAULT_SERIAL)]
    serial: String,
    #[arg(long, default_value = DEFAULT_TEMPLATE)]
    template: PathBuf,
    #[arg(long, default_value_t = 60)]
    enum_timeout: u64,
    #[arg(long, default_value_t = 30)]
    hold: u64,
    #[arg(long, default_value_t = 30)]
    fastboot_wait: u64,
    /// After a device-absent result, keep polling host transports for this
    /// bounded interval and resume the candidate plan if physical recovery
    /// makes Android ADB or Fastboot visible. Zero preserves immediate stop.
    #[arg(long, default_value_t = 0)]
    recovery_wait_secs: u64,
    #[arg(long)]
    usbmon: bool,
    /// Explicitly allow the selected ADB device to transition to Fastboot.
    /// This is enabled by default for the candidate plan.
    #[arg(long)]
    adb_reboot_to_fastboot: bool,
    /// Keep the plan passive when Android ADB is the initial state.
    #[arg(long, conflicts_with = "adb_reboot_to_fastboot")]
    no_adb_reboot_to_fastboot: bool,
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

#[derive(Parser, Debug)]
struct StatusArgs {
    #[arg(long, default_value = DEFAULT_SERIAL)]
    serial: String,
}

fn command_output_text(program: &str, arguments: &[&str]) -> io::Result<(bool, String)> {
    let output = Command::new(program).args(arguments).output()?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok((output.status.success(), text))
}

fn adb_state_from_listing(serial: &str, listing: &str) -> Option<String> {
    listing.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let found_serial = fields.next()?;
        let state = fields.next()?;
        (found_serial == serial).then(|| state.to_owned())
    })
}

fn classify_device_state(
    adb_state: Option<&str>,
    fastboot: bool,
    fullerene_usb: bool,
    android_usb: bool,
) -> DeviceState {
    if fullerene_usb {
        DeviceState::FullereneUsbAvailable
    } else if fastboot {
        DeviceState::FastbootAvailable
    } else if adb_state == Some("device") {
        DeviceState::AndroidAdbAvailable
    } else if adb_state.is_some() || android_usb {
        DeviceState::UnknownUsbState
    } else {
        DeviceState::DeviceAbsent
    }
}

fn observe_host(serial: &str) -> io::Result<HostObservation> {
    let (_, adb_devices) = command_output_text("adb", &["devices", "-l"])?;
    let (_, fastboot_devices) = command_output_text("fastboot", &["devices", "-l"])?;
    let (_, lsusb) = command_output_text("lsusb", &[])?;
    let adb_state = adb_state_from_listing(serial, &adb_devices);
    let fullerene_usb = lsusb.lines().any(|line| line.contains(FULLERENE_USB));
    let bootloader_usb = lsusb.lines().any(|line| line.contains(BOOTLOADER_USB));
    let android_usb = lsusb
        .lines()
        .any(|line| line.contains(ANDROID_FALLBACK_USB));
    let fastboot = fastboot_devices
        .lines()
        .any(|line| line.split_whitespace().next() == Some(serial));
    let state = classify_device_state(
        adb_state.as_deref(),
        fastboot,
        fullerene_usb,
        android_usb || bootloader_usb,
    );
    Ok(HostObservation {
        state,
        adb_state,
        adb_devices,
        fastboot_devices,
        lsusb,
        fastboot,
        fullerene_usb,
        bootloader_usb,
        android_usb,
    })
}

fn write_host_observation(
    run_dir: &Path,
    label: &str,
    observation: &HostObservation,
) -> io::Result<()> {
    let mut summary = String::new();
    summary.push_str(&format!("state={}\n", observation.state.as_str()));
    summary.push_str(&format!(
        "adb_state={}\n",
        observation.adb_state.as_deref().unwrap_or("absent")
    ));
    summary.push_str(&format!("fastboot_present={}\n", observation.fastboot));
    summary.push_str(&format!(
        "fullerene_usb_present={}\n",
        observation.fullerene_usb
    ));
    summary.push_str(&format!(
        "bootloader_usb_present={}\n",
        observation.bootloader_usb
    ));
    summary.push_str(&format!(
        "android_usb_present={}\n",
        observation.android_usb
    ));
    fs::write(run_dir.join(format!("{label}-summary.txt")), summary)?;
    fs::write(
        run_dir.join(format!("{label}-adb-devices.txt")),
        &observation.adb_devices,
    )?;
    fs::write(
        run_dir.join(format!("{label}-fastboot-devices.txt")),
        &observation.fastboot_devices,
    )?;
    fs::write(
        run_dir.join(format!("{label}-lsusb.txt")),
        &observation.lsusb,
    )?;
    Ok(())
}

fn append_host_timeline(
    timeline: &mut File,
    elapsed: Duration,
    observation: &HostObservation,
) -> io::Result<()> {
    writeln!(
        timeline,
        "[{elapsed:?}] state={}",
        observation.state.as_str()
    )?;
    writeln!(
        timeline,
        "adb_state={} fastboot={} fullerene={} bootloader_usb={} android_usb={}",
        observation.adb_state.as_deref().unwrap_or("absent"),
        observation.fastboot,
        observation.fullerene_usb,
        observation.bootloader_usb,
        observation.android_usb,
    )?;
    timeline.write_all(observation.lsusb.as_bytes())?;
    if !observation.lsusb.ends_with('\n') {
        timeline.write_all(b"\n")?;
    }
    timeline.flush()
}

fn record_repo_state(workspace: &Path, run_dir: &Path) -> io::Result<()> {
    let mut output = String::new();
    for (label, arguments) in [
        ("git-head", vec!["rev-parse", "HEAD"]),
        ("git-branch", vec!["branch", "--show-current"]),
        ("git-status", vec!["status", "--short", "--branch"]),
        ("git-diff-stat", vec!["diff", "--stat", "--"]),
    ] {
        let command = Command::new("git")
            .args(&arguments)
            .current_dir(workspace)
            .output()?;
        let mut text = String::from_utf8_lossy(&command.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&command.stderr));
        fs::write(run_dir.join(format!("{label}.txt")), &text)?;
        output.push_str(&format!("[{label}]\n{text}\n"));
    }
    let dirty_diff = Command::new("git")
        .args(["diff", "HEAD", "--binary", "--"])
        .current_dir(workspace)
        .output()?;
    if !dirty_diff.status.success() {
        return Err(io::Error::other(format!(
            "git diff HEAD failed: {}",
            String::from_utf8_lossy(&dirty_diff.stderr).trim()
        )));
    }
    let dirty_diff_path = run_dir.join("dirty-diff.patch");
    fs::write(&dirty_diff_path, &dirty_diff.stdout)?;
    let dirty_diff_sha256 = sha256(&dirty_diff_path)?;
    fs::write(
        run_dir.join("dirty-diff.sha256"),
        format!("{dirty_diff_sha256}  dirty-diff.patch\n"),
    )?;
    output.push_str(&format!("[dirty-diff-sha256]\n{dirty_diff_sha256}\n\n"));
    fs::write(run_dir.join("repo-state.txt"), output)
}

fn record_boot_template(template: &Path, run_dir: &Path) -> io::Result<()> {
    let template_sha256 = sha256(template)?;
    fs::write(
        run_dir.join("boot-template.sha256"),
        format!("{template_sha256}  {}\n", template.display()),
    )
}

fn record_fullerene_environment(run_dir: &Path, build: &CommandSpec) -> io::Result<()> {
    let mut inherited = BTreeMap::new();
    for (key, value) in std::env::vars() {
        if key.starts_with("FULLERENE_") {
            inherited.insert(key, value);
        }
    }
    let mut text = String::from("inherited_from_harness_process:\n");
    if inherited.is_empty() {
        text.push_str("<none>\n");
    } else {
        for (key, value) in inherited {
            text.push_str(&format!("{key}={value}\n"));
        }
    }
    text.push_str("\neffective_build_child_environment:\n");
    if build.envs.is_empty() {
        text.push_str("<none>\n");
    } else {
        for (key, value) in &build.envs {
            text.push_str(&format!("{key}={value}\n"));
        }
    }
    text.push_str("\ninherited_fullerene_environment_sanitized=true\n");
    fs::write(run_dir.join("fullerene-build-env.txt"), text)
}

fn record_command_spec(run_dir: &Path, label: &str, spec: &CommandSpec) -> io::Result<()> {
    let mut text = format!(
        "program={}\ncurrent_dir={}\n",
        spec.program,
        spec.current_dir.display()
    );
    for (index, argument) in spec.arguments.iter().enumerate() {
        text.push_str(&format!("arg[{index}]={argument}\n"));
    }
    for (key, value) in &spec.envs {
        text.push_str(&format!("env.{key}={value}\n"));
    }
    fs::write(run_dir.join(format!("{label}-command.txt")), text)
}

fn next_experiment_for_classification(classification: &str) -> &'static str {
    match classification {
        "fullerene-usb-1234:0001-descriptor-read-success" => {
            "none: Fullerene-owned 1234:0001 descriptor verified; preserve artifacts and repeat once for confirmation"
        }
        "device-absent" => {
            "manual-recovery-required: host cannot see a Bramble transport; recover the handset physically before another bounded run\ncandidate-plan=normal-android-init-dma-cache-maintenance\ncandidate-order=pre-dtb,post-dtb\ncandidate-plan-command=cargo run -q -p flasks --bin bramble-usb -- candidates\nadb-reboot-to-fastboot=enabled-by-default\nallowed-device-operations=adb reboot bootloader; RAM-only fastboot boot\nforbidden-device-operations=flash; erase; readback; partition-write; unlock; slot-mutation; factory-reset; Android configfs\ncandidate-common-loop-flags=--android-init --adb-return --early-usb-handoff --entry-secure-wdt --direct-handoff --no-smmu --dma-cache-maintenance --start-after-connect --refresh-hsphy-power --hsphy-source-exact --usb2-source-exact-device-reset --usb2-source-susphy --usb2-source-exact-devten --usb2-source-devten-before-runstop --usb2-source-exact-cmd-guard --usb2-source-exact-runstop\ncandidate-profile-exclusions=--android-resource-order --signal-probe --signal-early-drop --skip-typec-spmi --observe-secs\ncandidate.pre-dtb.artifact=tmp/fullerene-bramble-android-init-pre-dtb-cache-maintenance-trace-init.img\ncandidate.pre-dtb.expected_sha256=bf72b5bed84d198ab09a79e854e32fea2bb7180d971ccdf921f9bf8bd304c51b\ncandidate.pre-dtb.changed_variable=normal Android-init USB handoff before DTB scan plus explicit DMA cache maintenance\ncandidate.pre-dtb.extra-loop-flag=--early-usb-before-dtb-scan\ncandidate.post-dtb.artifact=tmp/fullerene-bramble-android-init-post-dtb-cache-maintenance-trace-init.img\ncandidate.post-dtb.expected_sha256=d0b8e42e774fa7bb0e0972b3cd4cf10bdda506e2d0baa151e49e08029421d5d0\ncandidate.post-dtb.changed_variable=normal Android-init USB handoff after DTB scan plus explicit DMA cache maintenance\ncandidate.post-dtb.extra-loop-flag=none\naction-after-recovery=invoke the Rust candidate plan in order with the exact common profile above; permit only ADB-to-Fastboot and RAM-only fastboot boot\ndevice-operation-while-absent=none"
        }
        "google-logo-or-software-unrecoverable-suspected" => {
            "manual-recovery-required: host cannot see a Bramble transport; recover the handset physically before another bounded run"
        }
        "fastboot-fallback" => {
            "recheck-transport: probe returned to Fastboot without Fullerene USB; preserve this run before selecting one new source-backed variable"
        }
        "android-fallback" => {
            "source-audit-required: preserve the Android fallback and inspect the retained USB2 PHY/RX/SOF boundary before another one-variable run"
        }
        "usb-attach-without-registered-descriptor"
        | "usb-attach-or-descriptor-failure--62"
        | "usb-attach-or-descriptor-failure--71"
        | "usb-attach-or-descriptor-failure--110"
        | "fullerene-usb-present-descriptor-read-failure" => {
            "source-audit-required: preserve the attach/descriptor boundary and inspect USB2 PHY RX/SOF or event ingress; do not repeat downstream EP0/TRB permutations"
        }
        "build-or-audit-failure" => {
            "build-fix-required: inspect the preserved build.log and image audit before any device operation"
        }
        "fastboot-boot-command-failed" => {
            "transport-audit-required: inspect the preserved boot command and Fastboot output; do not retry until the failure is understood"
        }
        "artifact-sha256-mismatch" => {
            "artifact-fix-required: rebuilt candidate SHA differs from the tracked expected image; do not issue fastboot boot"
        }
        _ => {
            "source-audit-required: preserve this bounded result and choose one new source-backed variable before another physical run"
        }
    }
}

fn write_classification(run_dir: &Path, classification: &str) -> io::Result<()> {
    fs::write(
        run_dir.join("classification.txt"),
        format!("classification={classification}\n"),
    )?;
    if !run_dir.join("next-experiment.txt").exists() {
        write_next_experiment(run_dir, next_experiment_for_classification(classification))?;
    }
    Ok(())
}

fn write_next_experiment(run_dir: &Path, recommendation: &str) -> io::Result<()> {
    fs::write(
        run_dir.join("next-experiment.txt"),
        format!("{recommendation}\n"),
    )
}

fn write_device_absent_recovery_plan(run_dir: &Path, workspace: &Path) -> io::Result<()> {
    let candidates = [
        (
            "pre-dtb",
            "tmp/fullerene-bramble-android-init-pre-dtb-cache-maintenance-trace-init.img",
            "bf72b5bed84d198ab09a79e854e32fea2bb7180d971ccdf921f9bf8bd304c51b",
            "normal Android-init USB handoff before DTB scan plus explicit DMA cache maintenance",
        ),
        (
            "post-dtb",
            "tmp/fullerene-bramble-android-init-post-dtb-cache-maintenance-trace-init.img",
            "d0b8e42e774fa7bb0e0972b3cd4cf10bdda506e2d0baa151e49e08029421d5d0",
            "normal Android-init USB handoff after DTB scan plus explicit DMA cache maintenance",
        ),
    ];
    let mut text = String::from(
        "manual-recovery-required: host sees no Bramble transport; after physical recovery, rerun the bounded Rust loop; no device-side operation was issued\n"
            .to_owned(),
    );
    text.push_str("candidate-plan=normal-android-init-dma-cache-maintenance\n");
    text.push_str("candidate-order=pre-dtb,post-dtb\n");
    text.push_str(
        "candidate-plan-command=cargo run -q -p flasks --bin bramble-usb -- candidates\n",
    );
    text.push_str(
        "autonomous-resume-option=cargo run -q -p flasks --bin bramble-usb -- candidates --recovery-wait-secs 900\n",
    );
    text.push_str("adb-reboot-to-fastboot=enabled-by-default\n");
    text.push_str("allowed-device-operations=adb reboot bootloader; RAM-only fastboot boot\n");
    text.push_str(
        "forbidden-device-operations=flash; erase; readback; partition-write; unlock; slot-mutation; factory-reset; Android configfs\n",
    );
    text.push_str("candidate-common-loop-flags=--android-init --adb-return --early-usb-handoff --entry-secure-wdt --direct-handoff --no-smmu --dma-cache-maintenance --start-after-connect --refresh-hsphy-power --hsphy-source-exact --usb2-source-exact-device-reset --usb2-source-susphy --usb2-source-exact-devten --usb2-source-devten-before-runstop --usb2-source-exact-cmd-guard --usb2-source-exact-runstop\n");
    text.push_str("candidate-profile-exclusions=--android-resource-order --signal-probe --signal-early-drop --skip-typec-spmi --observe-secs\n");
    for (index, (name, relative_path, expected_sha256, changed_variable)) in
        candidates.iter().enumerate()
    {
        let path = workspace.join(relative_path);
        let observed_sha256 = if path.is_file() {
            sha256(&path)?
        } else {
            "missing".to_owned()
        };
        let status = if observed_sha256 == *expected_sha256 {
            "ready"
        } else if observed_sha256 == "missing" {
            "missing"
        } else {
            "sha256-mismatch"
        };
        text.push_str(&format!(
            "candidate[{index}].name={name}\ncandidate[{index}].artifact={relative_path}\ncandidate[{index}].expected_sha256={expected_sha256}\ncandidate[{index}].observed_sha256={observed_sha256}\ncandidate[{index}].status={status}\ncandidate[{index}].changed_variable={changed_variable}\n"
        ));
        text.push_str(&format!(
            "candidate[{index}].extra-loop-flag={}\n",
            if *name == "pre-dtb" {
                "--early-usb-before-dtb-scan"
            } else {
                "none"
            }
        ));
    }
    text.push_str(
        "action-after-recovery=invoke the Rust candidate plan in order with the exact common profile above; permit only ADB-to-Fastboot and RAM-only fastboot boot\n",
    );
    text.push_str("device-operation-while-absent=none\n");
    write_next_experiment(run_dir, text.trim_end())
}

fn loop_args_debug_fields(text: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    let mut pending: Option<(String, String)> = None;
    for line in text.lines() {
        let line = line.trim();
        let field = line.split_once(':').and_then(|(name, value)| {
            if name.is_empty()
                || !name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
            {
                None
            } else {
                Some((
                    name.to_owned(),
                    value.trim().trim_end_matches(',').to_owned(),
                ))
            }
        });
        if let Some((name, value)) = field {
            if let Some((pending_name, pending_value)) = pending.take() {
                fields.insert(pending_name, pending_value);
            }
            pending = Some((name, value));
        } else if let Some((_, value)) = pending.as_mut() {
            value.push_str(line.trim_end_matches(','));
        }
        if pending
            .as_ref()
            .is_some_and(|(_, value)| value.matches('(').count() <= value.matches(')').count())
        {
            let (name, value) = pending.take().unwrap();
            fields.insert(name, value);
        }
    }
    if let Some((name, value)) = pending {
        fields.insert(name, value);
    }
    fields
}

fn manifest_value(value: &str) -> String {
    value
        .strip_prefix("Some(")
        .and_then(|value| value.strip_suffix(')'))
        .unwrap_or(value)
        .trim_matches('"')
        .to_owned()
}

fn append_unlisted_experiment_variables(args: &LoopArgs, variables: &mut Vec<String>) {
    let actual = loop_args_debug_fields(&format!("{args:#?}"));
    let defaults = loop_args_debug_fields(&format!("{:#?}", LoopArgs::default()));
    for (name, value) in actual {
        if defaults.get(&name) == Some(&value) {
            continue;
        }
        let key = name.replace('_', "-");
        if variables
            .iter()
            .any(|variable| variable.split_once('=').is_some_and(|(key, _)| key == name))
            || variables.iter().any(|variable| {
                variable
                    .split_once('=')
                    .is_some_and(|(variable_key, _)| variable_key == key)
            })
        {
            continue;
        }
        variables.push(format!("{key}={}", manifest_value(&value)));
    }
}

fn experiment_manifest(args: &LoopArgs) -> String {
    let mut variables = Vec::new();
    if let Some(route) = args.irq_route {
        variables.push(format!("irq-route={}", route.as_str()));
    }
    if args.direct_handoff {
        variables.push("direct-handoff=true".to_owned());
    }
    if args.no_smmu {
        variables.push("no-smmu=true".to_owned());
    }
    if args.no_core_reset {
        variables.push("no-core-reset=true".to_owned());
    }
    if args.super_speed {
        variables.push("super-speed=true".to_owned());
    }
    if args.hsphy_source_exact {
        variables.push("hsphy-source-exact=true".to_owned());
    }
    if args.hsphy_xbl_exact {
        variables.push("hsphy-xbl-exact=true".to_owned());
    }
    if args.xbl_hs_phy_table {
        variables.push("xbl-hs-phy-table=true".to_owned());
    }
    if args.usb2_source_susphy {
        variables.push("usb2-source-susphy=true".to_owned());
    }
    if args.usb2_source_exact_devten {
        variables.push("usb2-source-exact-devten=true".to_owned());
    }
    if args.usb2_source_devten_before_runstop {
        variables.push("usb2-source-devten-before-runstop=true".to_owned());
    }
    if args.usb2_source_exact_cmd_guard {
        variables.push("usb2-source-exact-cmd-guard=true".to_owned());
    }
    if args.usb2_source_exact_runstop {
        variables.push("usb2-source-exact-runstop=true".to_owned());
    }
    if args.usb2_source_phy_setup {
        variables.push("usb2-source-phy-setup=true".to_owned());
    }
    if args.usb2_source_exact_device_reset {
        variables.push("usb2-source-exact-device-reset=true".to_owned());
    }
    if args.usb2_qpr1_utmi_post_reset_only {
        variables.push("usb2-qpr1-utmi-post-reset-only=true".to_owned());
    }
    if args.usb2_android_dbm_reset {
        variables.push("usb2-android-dbm-reset=true".to_owned());
    }
    if args.min_runstop_delay {
        variables.push("min-runstop-delay=true".to_owned());
    }
    if args.gadget_restart_at_runstop {
        variables.push("gadget-restart-at-runstop=true".to_owned());
    }
    if args.start_after_connect {
        variables.push("start-after-connect=true".to_owned());
    }
    if args.dcfg_ignstrmpp {
        variables.push("dcfg-ignstrmpp=true".to_owned());
    }
    if args.signal_probe {
        variables.push("signal-probe=true".to_owned());
    }
    if args.signal_smmu_state {
        variables.push("signal-smmu-state=true".to_owned());
    }
    if args.signal_link_state {
        variables.push("signal-link-state=true".to_owned());
    }
    if args.signal_raw_link {
        variables.push("signal-raw-link=true".to_owned());
    }
    if let Some(code) = args.signal_early_drop {
        variables.push(format!("signal-early-drop={code}"));
    }
    if args.signal_pre_drop {
        variables.push("signal-pre-drop=true".to_owned());
    }
    if args.signal_heartbeat {
        variables.push("signal-heartbeat=true".to_owned());
    }
    if args.usbmon {
        variables.push("usbmon=true".to_owned());
    }
    // Keep the historical readable names above, but derive any newly added
    // LoopArgs fields from the actual/default Debug snapshots. This prevents
    // a new readout, gate, timing, or safety-affecting option from sharing an
    // old experiment_id merely because this hand-maintained list was not
    // updated at the same time.
    append_unlisted_experiment_variables(args, &mut variables);
    let profile = if variables.is_empty() {
        "baseline".to_owned()
    } else {
        variables.join(",")
    };
    let changed_variable = if variables.len() == 1 {
        variables[0].clone()
    } else if variables.is_empty() {
        "baseline".to_owned()
    } else {
        format!("combined-profile ({profile})")
    };
    let hypothesis = if args.irq_route.is_some() {
        "the selected DWC3 resource route changes handoff ownership/timing"
    } else {
        "the selected Fullerene USB build profile reaches a host-visible device"
    };
    format!(
        "experiment_id={profile}\nprofile={profile}\nmode={}\nhypothesis={hypothesis}\nchanged_variable={changed_variable}\nexpected_discriminator=host sees 1234:0001 and reads a Device Descriptor; otherwise preserve exact attach/descriptor errno and recovery state\nsafety=ADB-to-Fastboot and fastboot boot only; no flash, erase, readback, unlock, slot, reset, or Android configfs\nsources=flasks/src/bin/bramble-usb.rs::build_command,experiment_manifest,LoopArgs::default\n",
        mode_name(args),
    )
}

fn write_experiment_manifest(run_dir: &Path, args: &LoopArgs) -> io::Result<()> {
    fs::write(
        run_dir.join("experiment-manifest.txt"),
        experiment_manifest(args),
    )
}

fn kernel_log_has_non_android_attach(log: &str) -> bool {
    let mut pending: Option<(String, bool)> = None;
    for line in log.lines() {
        let Some(path_start) = line.find("usb ") else {
            continue;
        };
        let path = line[path_start + 4..].split(':').next().unwrap_or_default();
        if path.is_empty() {
            continue;
        }

        let new_device = line.contains("new high-speed USB device")
            || line.contains("new full-speed USB device")
            || line.contains("new SuperSpeed USB device");
        if new_device {
            if let Some((_, non_android)) = pending.take() {
                if non_android {
                    return true;
                }
            }
            pending = Some((path.to_owned(), true));
            continue;
        }

        let Some((pending_path, non_android)) = pending.as_mut() else {
            continue;
        };
        if pending_path != path {
            continue;
        }
        if line.contains("idVendor=18d1,idProduct=4ee7")
            || line.contains("idVendor=18d1, idProduct=4ee7")
        {
            *non_android = false;
        }
    }
    pending.is_some_and(|(_, non_android)| non_android)
}

fn classify_postboot_result(
    observation: &HostObservation,
    kernel_log: Option<&str>,
    descriptor_ok: Option<bool>,
) -> &'static str {
    if observation.fullerene_usb {
        return match descriptor_ok {
            Some(true) => "fullerene-usb-1234:0001-descriptor-read-success",
            Some(false) => "fullerene-usb-present-descriptor-read-failure",
            None => "fullerene-usb-present-descriptor-unverified",
        };
    }
    // Prefer a host kernel boundary over the final recovery state. Android
    // is expected to return after a failed probe, but that must not erase the
    // evidence that the temporary image reached HS/SS attach or a descriptor
    // error before recovery.
    if let Some(log) = kernel_log {
        if log.contains("error -110") {
            return "usb-attach-or-descriptor-failure--110";
        }
        if log.contains("error -71") {
            return "usb-attach-or-descriptor-failure--71";
        }
        if log.contains("error -62") {
            return "usb-attach-or-descriptor-failure--62";
        }
        if kernel_log_has_non_android_attach(log) {
            return "usb-attach-without-registered-descriptor";
        }
    }
    if observation.android_usb || observation.adb_state.as_deref() == Some("device") {
        return "android-fallback";
    }
    if observation.state == DeviceState::FastbootAvailable {
        return "fastboot-fallback";
    }
    DeviceState::GoogleLogoSuspected.as_str()
}

struct JournalGuard {
    child: Option<Child>,
    run_dir: PathBuf,
    start_iso: String,
}

struct UsbmonGuard {
    child: Option<Child>,
    run_dir: PathBuf,
    capture_path: PathBuf,
}

impl UsbmonGuard {
    fn start(run_dir: &Path) -> io::Result<Self> {
        let source = File::open("/dev/usbmon0").map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("cannot open passive usbmon source /dev/usbmon0: {error}"),
            )
        })?;
        let capture_path = run_dir.join("usbmon-all.bin");
        let output = File::create(&capture_path)?;
        let child = Command::new("cat")
            .stdin(Stdio::from(source))
            .stdout(Stdio::from(output))
            .stderr(Stdio::null())
            .spawn()?;
        fs::write(
            run_dir.join("usbmon-source.txt"),
            "source=/dev/usbmon0\nmode=all-usb-buses\nobservation=passive\n",
        )?;
        Ok(Self {
            child: Some(child),
            run_dir: run_dir.to_owned(),
            capture_path,
        })
    }

    fn stop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let kill_result = child.kill();
        let wait_result = child.wait();
        let mut status = String::from("source=/dev/usbmon0\n");
        status.push_str("mode=all-usb-buses\nobservation=passive\n");
        status.push_str(&format!("kill={kill_result:?}\nwait={wait_result:?}\n"));
        if let Ok(metadata) = fs::metadata(&self.capture_path) {
            status.push_str(&format!("bytes={}\n", metadata.len()));
            if let Ok(sha) = sha256(&self.capture_path) {
                status.push_str(&format!("sha256={sha}\n"));
                let _ = fs::write(
                    self.run_dir.join("usbmon-all.sha256"),
                    format!("{sha}  {}\n", self.capture_path.display()),
                );
            }
        }
        if let Ok(summary) = usbmon_summary(&self.capture_path) {
            let _ = fs::write(self.run_dir.join("usbmon-summary.txt"), summary);
        }
        let _ = fs::write(self.run_dir.join("usbmon-status.txt"), status);
    }
}

impl Drop for UsbmonGuard {
    fn drop(&mut self) {
        self.stop();
    }
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
        CommandKind::Candidates(args) => run_candidates(&workspace, args),
        CommandKind::Status(args) => run_status(args),
        CommandKind::Trace(args) => run_trace(args),
    }
}

fn run_status(args: StatusArgs) -> io::Result<()> {
    let observation = observe_host(&args.serial)?;
    println!("serial={}", args.serial);
    println!("state={}", observation.state.as_str());
    println!(
        "adb_state={} fastboot={} fullerene={} bootloader_usb={} android_usb={}",
        observation.adb_state.as_deref().unwrap_or("absent"),
        observation.fastboot,
        observation.fullerene_usb,
        observation.bootloader_usb,
        observation.android_usb,
    );
    Ok(())
}

fn normal_android_candidate_loop_args(args: &CandidatesArgs, pre_dtb: bool) -> LoopArgs {
    LoopArgs {
        serial: args.serial.clone(),
        template: args.template.clone(),
        enum_timeout: args.enum_timeout,
        hold: args.hold,
        fastboot_wait: args.fastboot_wait,
        usbmon: args.usbmon,
        adb_reboot_to_fastboot: args.adb_reboot_to_fastboot,
        no_adb_reboot_to_fastboot: args.no_adb_reboot_to_fastboot,
        android_init: true,
        early_usb_handoff: true,
        early_usb_before_dtb_scan: pre_dtb,
        entry_secure_wdt: true,
        adb_return: true,
        direct_handoff: true,
        no_smmu: true,
        dma_cache_maintenance: true,
        refresh_hsphy_power: true,
        start_after_connect: true,
        hsphy_source_exact: true,
        usb2_source_exact_device_reset: true,
        usb2_source_susphy: true,
        usb2_source_exact_devten: true,
        usb2_source_devten_before_runstop: true,
        usb2_source_exact_cmd_guard: true,
        usb2_source_exact_runstop: true,
        ..LoopArgs::default()
    }
}

fn wait_for_candidate_recovery(
    serial: &str,
    run_dir: &Path,
    requested_timeout_secs: u64,
) -> io::Result<HostObservation> {
    let timeout_secs = requested_timeout_secs.min(MAX_CANDIDATE_RECOVERY_WAIT_SECS);
    let mut timeline = File::create(run_dir.join("candidate-recovery-wait.tsv"))?;
    writeln!(
        timeline,
        "requested_timeout_secs={requested_timeout_secs}\neffective_timeout_secs={timeout_secs}"
    )?;
    writeln!(
        timeline,
        "elapsed_secs\tstate\tadb\tfastboot\tfullerene\tandroid"
    )?;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(timeout_secs);

    loop {
        let observation = observe_host(serial)?;
        writeln!(
            timeline,
            "{}\t{}\t{}\t{}\t{}\t{}",
            started.elapsed().as_secs(),
            observation.state.as_str(),
            observation.adb_state.as_deref().unwrap_or("absent"),
            observation.fastboot,
            observation.fullerene_usb,
            observation.android_usb,
        )?;
        if observation.state != DeviceState::DeviceAbsent {
            return Ok(observation);
        }

        let now = Instant::now();
        if now >= deadline {
            return Ok(observation);
        }
        thread::sleep(
            deadline
                .saturating_duration_since(now)
                .min(Duration::from_secs(1)),
        );
    }
}

fn run_candidates(workspace: &Path, mut args: CandidatesArgs) -> io::Result<()> {
    if args.template.is_relative() {
        args.template = workspace.join(&args.template);
    }
    let candidate_specs = [
        (
            "pre-dtb",
            true,
            "bf72b5bed84d198ab09a79e854e32fea2bb7180d971ccdf921f9bf8bd304c51b",
        ),
        (
            "post-dtb",
            false,
            "d0b8e42e774fa7bb0e0972b3cd4cf10bdda506e2d0baa151e49e08029421d5d0",
        ),
    ];
    if args.dry_run {
        println!("Bramble candidate plan (dry-run): pre-dtb -> post-dtb");
        println!(
            "recovery-wait-secs={} (max {})",
            args.recovery_wait_secs
                .min(MAX_CANDIDATE_RECOVERY_WAIT_SECS),
            MAX_CANDIDATE_RECOVERY_WAIT_SECS
        );
        for (name, pre_dtb, expected_sha256) in candidate_specs {
            println!("=== candidate: {name} ===");
            println!("expected-sha256={expected_sha256}");
            print_loop_command(&normal_android_candidate_loop_args(&args, pre_dtb));
        }
        return Ok(());
    }
    if !args.template.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("stock boot template not found: {}", args.template.display()),
        ));
    }

    let adb_reboot_to_fastboot =
        adb_reboot_to_fastboot_enabled(args.adb_reboot_to_fastboot, args.no_adb_reboot_to_fastboot);
    let recovery_wait_secs = args
        .recovery_wait_secs
        .min(MAX_CANDIDATE_RECOVERY_WAIT_SECS);
    let run_dir = create_run_dir(workspace, "fullerene-bramble-candidates")?;
    println!("Candidate plan logs: {}", run_dir.display());
    fs::write(
        run_dir.join("candidate-plan.txt"),
        format!(
            "candidate-order=pre-dtb,post-dtb\nprofile=normal-android-init-dma-cache-maintenance\nadb-reboot-to-fastboot=enabled-by-default\nrecovery-wait-secs={recovery_wait_secs}\nallowed-device-operations=adb reboot bootloader; RAM-only fastboot boot\nforbidden-device-operations=flash; erase; readback; partition-write; unlock; slot-mutation; factory-reset; Android configfs\ncommon-flags=--android-init --adb-return --early-usb-handoff --entry-secure-wdt --direct-handoff --no-smmu --dma-cache-maintenance --start-after-connect --refresh-hsphy-power --hsphy-source-exact --usb2-source-exact-device-reset --usb2-source-susphy --usb2-source-exact-devten --usb2-source-devten-before-runstop --usb2-source-exact-cmd-guard --usb2-source-exact-runstop\npre-dtb-extra-flag=--early-usb-before-dtb-scan\npre-dtb-expected-sha256=bf72b5bed84d198ab09a79e854e32fea2bb7180d971ccdf921f9bf8bd304c51b\npost-dtb-extra-flag=none\npost-dtb-expected-sha256=d0b8e42e774fa7bb0e0972b3cd4cf10bdda506e2d0baa151e49e08029421d5d0\nsafety=ADB-to-Fastboot and RAM-only fastboot boot only; no flash, erase, readback, unlock, slot, reset, or Android configfs\n"
        ),
    )?;
    fs::write(
        run_dir.join("candidate-ledger.tsv"),
        "step\tcandidate\tattempt\tclassification\tresult\n",
    )?;

    let mut index = 0;
    let mut retry_counts = [0_usize; 2];
    while index < candidate_specs.len() {
        let (name, pre_dtb, expected_sha256) = candidate_specs[index];
        let attempt = retry_counts[index] + 1;
        let attempt_name = if attempt == 1 {
            name.to_owned()
        } else {
            format!("{name}-retry-{}", attempt - 1)
        };
        fs::write(
            run_dir.join("next-experiment.txt"),
            format!(
                "step={}\ncandidate={}\nattempt={}\noperation=build, audit, fastboot boot, host observe, classify\n",
                index + 1,
                name,
                attempt
            ),
        )?;
        let loop_args = normal_android_candidate_loop_args(&args, pre_dtb);
        match run_loop_with_named_dir(
            workspace,
            loop_args,
            Some(&run_dir),
            Some(&attempt_name),
            Some(expected_sha256),
        ) {
            Ok(()) => {
                let mut ledger = fs::OpenOptions::new()
                    .append(true)
                    .open(run_dir.join("candidate-ledger.tsv"))?;
                writeln!(
                    ledger,
                    "{}\t{}\t{}\tfullerene-usb-1234:0001-descriptor-read-success\tpass",
                    index + 1,
                    name,
                    attempt
                )?;
                fs::write(
                    run_dir.join("next-experiment.txt"),
                    "none: Fullerene USB descriptor verification passed\n",
                )?;
                return Ok(());
            }
            Err(error) => {
                let child_dir = run_dir.join(&attempt_name);
                let classification = fs::read_to_string(child_dir.join("classification.txt"))
                    .unwrap_or_else(|_| "classification=run-error\n".to_owned())
                    .trim()
                    .strip_prefix("classification=")
                    .unwrap_or("run-error")
                    .to_owned();
                let mut ledger = fs::OpenOptions::new()
                    .append(true)
                    .open(run_dir.join("candidate-ledger.tsv"))?;
                writeln!(
                    ledger,
                    "{}\t{}\t{}\t{}\tfail",
                    index + 1,
                    name,
                    attempt,
                    classification
                )?;

                if matches!(
                    classification.as_str(),
                    "build-or-audit-failure"
                        | "fastboot-boot-command-failed"
                        | "artifact-sha256-mismatch"
                ) {
                    return Err(io::Error::other(format!(
                        "candidate {name} stopped after {classification}: {error}; logs: {}",
                        run_dir.display()
                    )));
                }

                let observation = observe_host(&args.serial)?;
                let was_device_absent = observation.state == DeviceState::DeviceAbsent;
                let observation = if was_device_absent && recovery_wait_secs > 0 {
                    eprintln!(
                        "candidate {name}: device absent; waiting up to {recovery_wait_secs}s for host-visible recovery"
                    );
                    wait_for_candidate_recovery(&args.serial, &run_dir, recovery_wait_secs)?
                } else {
                    observation
                };
                let recovered_from_absence =
                    was_device_absent && observation.state != DeviceState::DeviceAbsent;
                let can_retry_same_candidate = recovered_from_absence
                    && retry_counts[index] == 0
                    && !child_dir.join("boot-command.txt").is_file();
                match observation.state {
                    DeviceState::FastbootAvailable => {
                        if can_retry_same_candidate {
                            retry_counts[index] += 1;
                            eprintln!(
                                "candidate {name}: recovered before build/boot; retrying the same candidate"
                            );
                            continue;
                        }
                    }
                    DeviceState::AndroidAdbAvailable if adb_reboot_to_fastboot => {
                        ensure_fastboot_from_adb(&args.serial, args.fastboot_wait, &run_dir)?;
                        if can_retry_same_candidate {
                            retry_counts[index] += 1;
                            eprintln!(
                                "candidate {name}: recovered through Android ADB before build/boot; retrying the same candidate"
                            );
                            continue;
                        }
                    }
                    state => {
                        if state == DeviceState::DeviceAbsent {
                            fs::write(
                                run_dir.join("next-experiment.txt"),
                                format!(
                                    "manual-recovery-required: candidate {name} remained device-absent after bounded recovery wait\nrecovery-wait-expired-secs={recovery_wait_secs}\naction-after-recovery=rerun the candidate plan; permit only ADB-to-Fastboot and RAM-only fastboot boot\ndevice-operation-while-absent=none\n"
                                ),
                            )?;
                        }
                        return Err(io::Error::other(format!(
                            "candidate {name} stopped after {classification}; recovery state={}; logs: {}",
                            state.as_str(),
                            run_dir.display()
                        )));
                    }
                }
                index += 1;
            }
        }
    }

    fs::write(
        run_dir.join("next-experiment.txt"),
        "none: tracked normal Android-init candidate plan exhausted; preserve the ledger and return to source audit\n",
    )?;
    Err(io::Error::other(format!(
        "candidate plan exhausted without Fullerene USB; logs are under {}",
        run_dir.display()
    )))
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

fn prior_experiment_classification(
    workspace: &Path,
    experiment_id: &str,
) -> io::Result<Option<String>> {
    let tmp = workspace.join("tmp");
    let Ok(entries) = fs::read_dir(tmp) else {
        return Ok(None);
    };
    for entry in entries {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        let mut candidates = vec![path.clone()];
        if let Ok(children) = fs::read_dir(&path) {
            for child in children {
                let child = child?.path();
                if child.is_dir() {
                    candidates.push(child);
                }
            }
        }
        for candidate in candidates {
            let manifest = candidate.join("experiment-manifest.txt");
            let Ok(text) = fs::read_to_string(manifest) else {
                continue;
            };
            if !text
                .lines()
                .any(|line| line == format!("experiment_id={experiment_id}"))
            {
                continue;
            }
            let classification = fs::read_to_string(candidate.join("classification.txt"))
                .unwrap_or_else(|_| "classification=unknown-prior-result\n".to_owned());
            return Ok(Some(
                classification
                    .trim()
                    .strip_prefix("classification=")
                    .unwrap_or("unknown-prior-result")
                    .to_owned(),
            ));
        }
    }
    Ok(None)
}

fn run_matrix(workspace: &Path, mut args: MatrixArgs) -> io::Result<()> {
    if args.template.is_relative() {
        args.template = workspace.join(&args.template);
    }
    let routes = if args.routes.is_empty() {
        vec![
            Route::Controller,
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

    let adb_reboot_to_fastboot =
        adb_reboot_to_fastboot_enabled(args.adb_reboot_to_fastboot, args.no_adb_reboot_to_fastboot);

    let run_dir = create_run_dir(workspace, "fullerene-bramble-matrix")?;
    println!("Matrix logs: {}", run_dir.display());
    let plan = routes
        .iter()
        .enumerate()
        .map(|(index, route)| {
            format!(
                "step={}\nexperiment=irq-route={}\nhypothesis=DWC3 resource route '{}' changes ownership/timing at the Fullerene handoff boundary\nexpected=host sees 1234:0001 and reads a Device Descriptor; otherwise preserve the exact host errno and recovery state\n",
                index + 1,
                route.as_str(),
                route.as_str()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(run_dir.join("experiment-plan.txt"), plan)?;
    fs::write(
        run_dir.join("matrix-ledger.tsv"),
        "step\texperiment\tclassification\tresult\n",
    )?;
    for (index, route) in routes.into_iter().enumerate() {
        fs::write(
            run_dir.join("next-experiment.txt"),
            format!(
                "step={}\nexperiment=irq-route={}\noperation=build, audit, fastboot boot, host observe, classify\n",
                index + 1,
                route.as_str()
            ),
        )?;
        println!("=== route: {} ===", route.as_str());
        let loop_args = loop_args_for_route(&args, route);
        let experiment_id = experiment_manifest(&loop_args)
            .lines()
            .find_map(|line| line.strip_prefix("experiment_id="))
            .unwrap_or("unknown")
            .to_owned();
        if let Some(previous) = prior_experiment_classification(workspace, &experiment_id)? {
            let mut ledger = fs::OpenOptions::new()
                .append(true)
                .open(run_dir.join("matrix-ledger.tsv"))?;
            writeln!(
                ledger,
                "{}\tirq-route={}\t{}\tskip-duplicate",
                index + 1,
                route.as_str(),
                previous
            )?;
            eprintln!(
                "route {} already has experiment_id={experiment_id} ({previous}); skipping duplicate",
                route.as_str()
            );
            continue;
        }
        match run_loop_with_dir(workspace, loop_args, Some(&run_dir)) {
            Ok(()) => {
                let mut ledger = fs::OpenOptions::new()
                    .append(true)
                    .open(run_dir.join("matrix-ledger.tsv"))?;
                writeln!(
                    ledger,
                    "{}\tirq-route={}\tfullerene-usb-1234:0001-descriptor-read-success\tpass",
                    index + 1,
                    route.as_str()
                )?;
                fs::write(
                    run_dir.join("next-experiment.txt"),
                    "none: Fullerene USB descriptor verification passed\n",
                )?;
                println!("USB route matrix: PASS ({})", route.as_str());
                return Ok(());
            }
            Err(error) => {
                let classification =
                    fs::read_to_string(run_dir.join(route.as_str()).join("classification.txt"))
                        .unwrap_or_else(|_| "classification=run-error\n".to_owned())
                        .trim()
                        .strip_prefix("classification=")
                        .unwrap_or("run-error")
                        .to_owned();
                let mut ledger = fs::OpenOptions::new()
                    .append(true)
                    .open(run_dir.join("matrix-ledger.tsv"))?;
                writeln!(
                    ledger,
                    "{}\tirq-route={}\t{}\tfail",
                    index + 1,
                    route.as_str(),
                    classification
                )?;
                eprintln!("route {} failed: {error}", route.as_str());
                let recovery = if adb_reboot_to_fastboot {
                    ensure_fastboot_from_adb(&args.serial, args.fastboot_wait, &run_dir)
                } else {
                    wait_for_fastboot(&args.serial, args.fastboot_wait)
                };
                if let Err(recovery) = recovery {
                    return Err(io::Error::other(format!(
                        "route {} failed and Fastboot did not return: {recovery}; logs are under {}",
                        route.as_str(),
                        run_dir.display()
                    )));
                }
                eprintln!("Fastboot returned; trying the next route");
            }
        }
    }
    fs::write(
        run_dir.join("next-experiment.txt"),
        "none: bounded route matrix exhausted; return to source audit or add one new source-backed variable\n",
    )?;
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
        adb_reboot_to_fastboot: args.adb_reboot_to_fastboot,
        no_adb_reboot_to_fastboot: args.no_adb_reboot_to_fastboot,
        irq_route: Some(route),
        super_speed: args.super_speed,
        no_smmu: args.no_smmu,
        no_core_reset: args.no_core_reset,
        direct_handoff: matches!(route, Route::Controller),
        ..LoopArgs::default()
    }
}

fn run_loop(workspace: &Path, mut args: LoopArgs) -> io::Result<()> {
    if args.template.is_relative() {
        args.template = workspace.join(&args.template);
    }
    if args.normal
        && (args.super_speed
            || args.pullup_only
            || args.bare_pullup
            || args.stop_after_stage.is_some()
            || args.direct_handoff)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--normal cannot be combined with --super-speed, --pullup-only, --bare-pullup, --stop-after-stage, or --direct-handoff",
        ));
    }
    if args.direct_handoff && (args.pullup_only || args.bare_pullup) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--direct-handoff cannot be combined with --pullup-only or --bare-pullup",
        ));
    }
    if args.start_after_connect && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--start-after-connect requires --direct-handoff",
        ));
    }
    if args.event_ring_size_4096 && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--event-ring-size-4096 requires --direct-handoff",
        ));
    }
    if args.xbl_deferred_setup && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--xbl-deferred-setup requires --direct-handoff",
        ));
    }
    if args.xbl_ep0_in_data && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--xbl-ep0-in-data requires --direct-handoff",
        ));
    }
    if args.xbl_event_dma && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--xbl-event-dma requires --direct-handoff",
        ));
    }
    if args.xbl_ep0_config && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--xbl-ep0-config requires --direct-handoff",
        ));
    }
    if args.xbl_between_ep0 && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--xbl-between-ep0 requires --direct-handoff",
        ));
    }
    if args.xbl_post_endpoint_global && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--xbl-post-endpoint-global requires --direct-handoff",
        ));
    }
    if args.xbl_stock_ep0_dma && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--xbl-stock-ep0-dma requires --direct-handoff",
        ));
    }
    if args.xbl_raw_runstop && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--xbl-raw-runstop requires --direct-handoff",
        ));
    }
    if args.source_exact_runstop && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--source-exact-runstop requires --super-speed",
        ));
    }
    if args.ss_reassert_runstop && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-reassert-runstop requires --super-speed",
        ));
    }
    if args.ss_hold_runstop && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-hold-runstop requires --super-speed",
        ));
    }
    if args.ss_retry_setup && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-retry-setup requires --super-speed",
        ));
    }
    if args.ss_eager_setup && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-eager-setup requires --super-speed",
        ));
    }
    if args.ss_source_susphy && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-source-susphy requires --super-speed",
        ));
    }
    if args.ss_conndone_clear_hird && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-conndone-clear-hird requires --super-speed",
        ));
    }
    if args.ss_reassert_device_mode && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-reassert-device-mode requires --super-speed",
        ));
    }
    if args.ss_reassert_core_clocks && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-reassert-core-clocks requires --super-speed",
        ));
    }
    if args.ss_reassert_core_clocks_after_runstop && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-reassert-core-clocks-after-runstop requires --super-speed",
        ));
    }
    if args.ss_reassert_domain_after_runstop && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-reassert-domain-after-runstop requires --super-speed",
        ));
    }
    if args.ss_reassert_link_clocks_after_runstop && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-reassert-link-clocks-after-runstop requires --super-speed",
        ));
    }
    if args.ss_android_dbm_reset && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-android-dbm-reset requires --super-speed",
        ));
    }
    if args.ss_reassert_qmp_power && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-reassert-qmp-power requires --super-speed",
        ));
    }
    if args.ss_reassert_qmp_power_after_gctl && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-reassert-qmp-power-after-gctl requires --super-speed",
        ));
    }
    if args.ss_reinit_hs_phy && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-reinit-hs-phy requires --super-speed",
        ));
    }
    if args.ss_pre_qmp_phy_setup && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-pre-qmp-phy-setup requires --super-speed",
        ));
    }
    if args.ss_clear_qmp_autonomous && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-clear-qmp-autonomous requires --super-speed",
        ));
    }
    if args.ss_reassert_qmp_clocks && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-reassert-qmp-clocks requires --super-speed",
        ));
    }
    if args.ss_reassert_qmp_clocks_after_gctl && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-reassert-qmp-clocks-after-gctl requires --super-speed",
        ));
    }
    if args.ss_reassert_hs_phy_ref_after_gctl && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-reassert-hs-phy-ref-after-gctl requires --super-speed",
        ));
    }
    if args.ss_dis_sleep_mode_before_gadget && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-dis-sleep-mode-before-gadget requires --super-speed",
        ));
    }
    if args.ss_clear_qmp_autonomous_exact && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-clear-qmp-autonomous-exact requires --super-speed",
        ));
    }
    if args.ss_qmp_resume_wmb && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-qmp-resume-wmb requires --super-speed",
        ));
    }
    if args.ss_qmp_lfps_clear_wmb && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-qmp-lfps-clear-wmb requires --super-speed",
        ));
    }
    if args.ss_qmp_notify_disconnect && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-qmp-notify-disconnect requires --super-speed",
        ));
    }
    if args.ss_clear_vbus_override_before_qmp && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-clear-vbus-override-before-qmp requires --super-speed",
        ));
    }
    if args.ss_clear_keep_connect_before_stop && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-clear-keep-connect-before-stop requires --super-speed",
        ));
    }
    if args.ss_clear_usb3_susphy_before_qmp && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-clear-usb3-susphy-before-qmp requires --super-speed",
        ));
    }
    if args.ss_clear_usb3_susphy_before_runstop && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-clear-usb3-susphy-before-runstop requires --super-speed",
        ));
    }
    if args.ss_clear_usb3_susphy_after_runstop && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-clear-usb3-susphy-after-runstop requires --super-speed",
        ));
    }
    if args.ss_core_reset_at_runstop && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-core-reset-at-runstop requires --super-speed",
        ));
    }
    if args.ss_separate_setup_buffer && !args.super_speed && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-separate-setup-buffer requires --direct-handoff or --super-speed",
        ));
    }
    if args.ss_disable_gadget_irq_before_stop && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-disable-gadget-irq-before-stop requires --super-speed",
        ));
    }
    if args.ss_disable_ep0_before_stop && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-disable-ep0-before-stop requires --super-speed",
        ));
    }
    if args.ss_clear_gsi_stop_state && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-clear-gsi-stop-state requires --super-speed",
        ));
    }
    if args.ss_lfps_timer && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-lfps-timer requires --super-speed",
        ));
    }
    if args.ss_clear_ux_exit_px && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-clear-ux-exit-px requires --super-speed",
        ));
    }
    if args.ss_preserve_ref_clock_state && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-preserve-ref-clock-state requires --super-speed",
        ));
    }
    if args.ss_preserve_phy_state && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ss-preserve-phy-state requires --super-speed",
        ));
    }
    if args.dt_hird_threshold && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--dt-hird-threshold requires --direct-handoff",
        ));
    }
    if args.android_hs_lpm && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--android-hs-lpm requires --direct-handoff",
        ));
    }
    if args.android_lpm_errata && !args.android_hs_lpm {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--android-lpm-errata requires --android-hs-lpm",
        ));
    }
    if args.abl_shared_hs_phy && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--abl-shared-hs-phy requires --direct-handoff",
        ));
    }
    if args.abl_ep_config && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--abl-ep-config requires --direct-handoff",
        ));
    }
    if args.abl_command_params && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--abl-command-params requires --direct-handoff",
        ));
    }
    if args.abl_trb_flags && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--abl-trb-flags requires --direct-handoff",
        ));
    }
    if args.abl_setup_trb_buffer && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--abl-setup-trb-buffer requires --direct-handoff",
        ));
    }
    if args.abl_event_consume && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--abl-event-consume requires --direct-handoff",
        ));
    }
    if args.xbl_direction_trb && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--xbl-direction-trb requires --direct-handoff",
        ));
    }
    if args.xbl_trb_chain && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--xbl-trb-chain requires --direct-handoff",
        ));
    }
    if args.ep0_reset_clear_stall && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ep0-reset-clear-stall requires --direct-handoff",
        ));
    }
    if args.ep0_reset_clear_test_mode && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ep0-reset-clear-test-mode requires --direct-handoff",
        ));
    }
    if args.ep0_reset_callback_first && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ep0-reset-callback-first requires --direct-handoff",
        ));
    }
    if args.ep0_reset_android_state_order && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ep0-reset-android-state-order requires --direct-handoff",
        ));
    }
    if args.start_ungated && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--start-ungated requires --direct-handoff",
        ));
    }
    if args.event_ring_at_runstop && !args.direct_handoff && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--event-ring-at-runstop requires --direct-handoff with USB2 or SuperSpeed",
        ));
    }
    if args.gadget_restart_at_runstop && !args.direct_handoff && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--gadget-restart-at-runstop requires --direct-handoff or --super-speed",
        ));
    }
    if args.gadget_start_only_at_runstop
        && (!args.direct_handoff || !args.gadget_restart_at_runstop || args.super_speed)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--gadget-start-only-at-runstop requires direct USB2 --gadget-restart-at-runstop",
        ));
    }
    if args.clear_gsi_after_reset && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--clear-gsi-after-reset requires --direct-handoff",
        ));
    }
    if args.hsphy_source_exact && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--hsphy-source-exact requires --direct-handoff",
        ));
    }
    if args.hsphy_xbl_exact && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--hsphy-xbl-exact requires --direct-handoff",
        ));
    }
    if args.hsphy_xbl_exact && !args.hsphy_source_exact {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--hsphy-xbl-exact requires --hsphy-source-exact",
        ));
    }
    if args.hsphy_xbl_exact && !args.xbl_hs_phy_table {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--hsphy-xbl-exact requires --xbl-hs-phy-table",
        ));
    }
    if args.hsphy_xbl_exact && args.abl_shared_hs_phy {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--hsphy-xbl-exact cannot be combined with --abl-shared-hs-phy",
        ));
    }
    if args.hsphy_all_regulator_sets && (!args.direct_handoff || !args.refresh_hsphy_power) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--hsphy-all-regulator-sets requires direct --refresh-hsphy-power",
        ));
    }
    if args.hsphy_legacy_fallback && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--hsphy-legacy-fallback requires --direct-handoff",
        ));
    }
    if args.hsphy_before_reset && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--hsphy-before-reset requires --direct-handoff",
        ));
    }
    if args.hsphy_restore_suspend_n_after_runstop && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--hsphy-restore-suspend-n-after-runstop requires --direct-handoff",
        ));
    }
    if args.hsphy_restore_suspend_n_selected_after_runstop && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--hsphy-restore-suspend-n-selected-after-runstop requires --direct-handoff",
        ));
    }
    if args.ep0_initial_512 && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ep0-initial-512 requires --direct-handoff",
        ));
    }
    if args.clock_branches_rearm && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--clock-branches-rearm requires --direct-handoff",
        ));
    }
    if args.gadget_start_defaults_at_runstop && !args.direct_handoff && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--gadget-start-defaults-at-runstop requires --direct-handoff or --super-speed",
        ));
    }
    if args.min_runstop_delay && !args.direct_handoff && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--min-runstop-delay requires --direct-handoff or --super-speed",
        ));
    }
    if args.usb_core_hs_clock && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb-core-hs-clock requires --direct-handoff",
        ));
    }
    if args.clock_stable_delay_us.is_some() && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--clock-stable-delay-us requires --direct-handoff",
        ));
    }
    if args.android_block_reset && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--android-block-reset requires --direct-handoff",
        ));
    }
    if args.skip_usb2_phy_reset && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--skip-usb2-phy-reset requires --direct-handoff",
        ));
    }
    if args.dcfg_ignstrmpp && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--dcfg-ignstrmpp requires --direct-handoff",
        ));
    }
    if args.u2_freeclk_clear && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--u2-freeclk-clear requires --direct-handoff",
        ));
    }
    if args.u2_freeclk_set && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--u2-freeclk-set requires --direct-handoff",
        ));
    }
    if args.u2_freeclk_clear && args.u2_freeclk_set {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--u2-freeclk-clear and --u2-freeclk-set are mutually exclusive",
        ));
    }
    if args.usb2_susphy && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb2-susphy requires --direct-handoff",
        ));
    }
    if args.usb2_susphy_after_runstop && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb2-susphy-after-runstop requires --super-speed",
        ));
    }
    if args.usb2_source_susphy && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb2-source-susphy requires --direct-handoff",
        ));
    }
    if args.usb2_source_exact_devten && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb2-source-exact-devten requires --direct-handoff",
        ));
    }
    if args.usb2_source_devten_before_runstop && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb2-source-devten-before-runstop requires --direct-handoff",
        ));
    }
    if args.usb2_source_exact_cmd_guard && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb2-source-exact-cmd-guard requires --direct-handoff",
        ));
    }
    if args.usb2_source_exact_runstop && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb2-source-exact-runstop requires --direct-handoff",
        ));
    }
    if args.usb2_source_phy_setup && (!args.direct_handoff || args.super_speed) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb2-source-phy-setup requires direct USB2 handoff",
        ));
    }
    if args.usb2_dis_sleep_mode && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb2-dis-sleep-mode requires --direct-handoff",
        ));
    }
    if args.usb2_android_dbm_reset && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb2-android-dbm-reset requires --direct-handoff",
        ));
    }
    if args.usb2_source_exact_device_reset && (!args.direct_handoff || args.super_speed) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb2-source-exact-device-reset requires direct USB2 handoff",
        ));
    }
    if args.usb2_qpr1_utmi_post_reset_only && (!args.direct_handoff || args.super_speed) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb2-qpr1-utmi-post-reset-only requires direct USB2 handoff",
        ));
    }
    if args.usb2_preserve_phy_interface && (!args.direct_handoff || args.super_speed) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb2-preserve-phy-interface requires direct USB2 handoff",
        ));
    }
    if args.dcfg_lowspeed
        && (!args.direct_handoff || args.super_speed || args.dcfg_fullspeed || args.dcfg_superspeed)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--dcfg-lowspeed requires direct USB2 handoff and cannot be combined with full-speed or SuperSpeed",
        ));
    }
    if args.ep0_stall_flush && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ep0-stall-flush requires --direct-handoff",
        ));
    }
    if args.ep0_short_first_desc && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ep0-short-first-desc requires --direct-handoff",
        ));
    }
    if args.ep0_txfifo_fix && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--ep0-txfifo-fix requires --direct-handoff",
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
    if args.reset_resource && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--reset-resource requires --direct-handoff",
        ));
    }
    if args.reset_endpoints && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--reset-endpoints requires --direct-handoff",
        ));
    }
    let ss_signal_stage = args.super_speed && matches!(args.stop_after_stage, Some(13..=29));
    let ss_full_signal_probe = args.super_speed;
    if args.signal_probe && !args.direct_handoff && !ss_full_signal_probe && !ss_signal_stage {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-probe requires --direct-handoff, or a SuperSpeed handoff",
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
    if args.signal_prev_trace_gate.is_some()
        && !matches!(args.signal_prev_trace_gate, Some(1 | 2 | 3 | 4 | 5))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-prev-trace-gate must be 1 through 5",
        ));
    }
    if args.signal_prev_qmp_gate.is_some() && !matches!(args.signal_prev_qmp_gate, Some(1..=8)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-prev-qmp-gate must be 1 through 8",
        ));
    }
    if args.signal_ram_gate && !args.signal_dma_probe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-ram-gate requires --signal-dma-probe",
        ));
    }
    if args.u0_arm_probe && !args.signal_probe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--u0-arm-probe requires --signal-probe",
        ));
    }
    if args.wdt_bite_control && !args.signal_probe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--wdt-bite-control requires --signal-probe",
        ));
    }
    if let Some(value) = &args.swdd_fnid {
        let hex = value.trim_start_matches("0x");
        if u32::from_str_radix(hex, 16).is_err() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("--swdd-fnid must be a 32-bit hex value, got {value}"),
            ));
        }
    }
    if args.arm_blip && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--arm-blip requires --direct-handoff",
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
    if args.signal_cmd_gate.is_some() && !args.direct_handoff && !ss_signal_stage {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-cmd-gate requires --direct-handoff, or a SuperSpeed stage probe",
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
    if args.signal_dma_post_runstop && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-dma-post-runstop requires --direct-handoff",
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
    if args.xbl_deferred_setup
        && (args.start_after_connect || args.start_after_reset || args.start_at_connect_done)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--xbl-deferred-setup is mutually exclusive with the EP0 timing differentials",
        ));
    }
    if args.xbl_ep0_in_data && args.xbl_deferred_setup {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--xbl-ep0-in-data is mutually exclusive with --xbl-deferred-setup",
        ));
    }
    if args.xbl_between_ep0
        && (args.xbl_deferred_setup
            || args.start_after_connect
            || args.start_after_reset
            || args.start_at_connect_done)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--xbl-between-ep0 is mutually exclusive with deferred or post-link EP0 arming",
        ));
    }
    if args.xbl_post_endpoint_global && args.xbl_deferred_setup {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--xbl-post-endpoint-global is mutually exclusive with --xbl-deferred-setup",
        ));
    }
    if args.pullup_only && (args.no_smmu || args.no_core_reset || args.irq_route.is_some()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--pullup-only cannot be combined with IRQ, SMMU, or core-reset differentials",
        ));
    }
    if matches!(args.irq_route, Some(Route::Controller)) && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--irq-route controller requires --direct-handoff",
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
    if let Some(stage) = args.bare_pullup_stop_after {
        if !args.bare_pullup {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--bare-pullup-stop-after requires --bare-pullup",
            ));
        }
        if !(1..=4).contains(&stage) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("--bare-pullup-stop-after must be 1..=4, got {stage}"),
            ));
        }
    }
    if args.hyper_bare && !args.bare_pullup {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--hyper-bare requires --bare-pullup",
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
        && !matches!(args.stop_after_stage, Some(13..=29))
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
    if args.preserve_fastboot_runstop && (!args.direct_handoff || !args.no_core_reset) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--preserve-fastboot-runstop requires --direct-handoff and --no-core-reset",
        ));
    }
    if args.qmp_lane.is_some() && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--qmp-lane requires --super-speed",
        ));
    }
    if args.xbl_qmp_table && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--xbl-qmp-table requires --super-speed",
        ));
    }
    if args.xbl_hs_phy_table && !(args.super_speed || args.direct_handoff) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--xbl-hs-phy-table requires --super-speed or --direct-handoff",
        ));
    }
    if args.qmp_phase_stop.is_some() && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--qmp-phase-stop requires --super-speed",
        ));
    }
    if args.qmp_phase_stop.is_some() && args.stop_after_stage.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--qmp-phase-stop cannot be combined with --stop-after-stage",
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
    run_loop_with_named_dir(workspace, args, matrix_dir, None, None)
}

fn run_loop_with_named_dir(
    workspace: &Path,
    args: LoopArgs,
    matrix_dir: Option<&Path>,
    child_name: Option<&str>,
    expected_sha256: Option<&str>,
) -> io::Result<()> {
    let adb_reboot_to_fastboot =
        adb_reboot_to_fastboot_enabled(args.adb_reboot_to_fastboot, args.no_adb_reboot_to_fastboot);
    let run_dir = match matrix_dir {
        Some(matrix_dir) => create_child_run_dir(
            matrix_dir,
            child_name.unwrap_or_else(|| args.irq_route.map_or("loop", Route::as_str)),
        )?,
        None => create_run_dir(workspace, "fullerene-bramble-loop")?,
    };
    let output = run_dir.join("fullerene-bramble-boot.img");
    println!("Bramble serial: {}", args.serial);
    println!("Stock template: {}", args.template.display());
    println!("Boot artifact: {}", output.display());
    println!("Logs: {}", run_dir.display());

    record_repo_state(workspace, &run_dir)?;
    record_boot_template(&args.template, &run_dir)?;
    fs::write(
        run_dir.join("loop-args-debug.txt"),
        format!("mode={}\n{args:#?}\n", mode_name(&args)),
    )?;
    write_experiment_manifest(&run_dir, &args)?;
    let initial_observation = observe_host(&args.serial)?;
    write_host_observation(&run_dir, "host-state-before", &initial_observation)?;
    println!(
        "Initial host state: {} (adb={}, fastboot={}, fullerene={}, android-usb={})",
        initial_observation.state.as_str(),
        initial_observation.adb_state.as_deref().unwrap_or("absent"),
        initial_observation.fastboot,
        initial_observation.fullerene_usb,
        initial_observation.android_usb,
    );

    // A previous probe normally recovers through Android before the next
    // iteration. The safe ADB-to-Fastboot transition is automatic for a
    // selected, authorized ADB device; --no-adb-reboot-to-fastboot retains a
    // passive Fastboot-only mode for diagnostic runs.
    if initial_observation.state == DeviceState::FullereneUsbAvailable {
        write_classification(&run_dir, DeviceState::FullereneUsbAvailable.as_str())?;
        write_next_experiment(
            &run_dir,
            "verification-required: Fullerene USB is already present; preserve this run and read its owned descriptors before any new boot",
        )?;
        return Err(io::Error::other(format!(
            "Fullerene USB is already present; refusing to issue fastboot boot; logs: {}",
            run_dir.display()
        )));
    }
    if initial_observation.state == DeviceState::AndroidAdbAvailable && !adb_reboot_to_fastboot {
        write_classification(&run_dir, DeviceState::AndroidAdbAvailable.as_str())?;
        write_next_experiment(
            &run_dir,
            "safe-transition-available: rerun without --no-adb-reboot-to-fastboot to use adb reboot bootloader, then bounded fastboot boot",
        )?;
        return Err(io::Error::other(format!(
            "Android ADB is available but --no-adb-reboot-to-fastboot was selected; logs: {}",
            run_dir.display()
        )));
    }
    let transport_result = if adb_reboot_to_fastboot {
        ensure_fastboot_from_adb(&args.serial, args.fastboot_wait, &run_dir)
    } else {
        wait_for_fastboot(&args.serial, args.fastboot_wait)
    };
    if let Err(error) = transport_result {
        let timeout_observation = observe_host(&args.serial)?;
        write_host_observation(
            &run_dir,
            "host-state-transport-timeout",
            &timeout_observation,
        )?;
        let classification = if timeout_observation.state == DeviceState::DeviceAbsent {
            DeviceState::DeviceAbsent.as_str()
        } else {
            timeout_observation.state.as_str()
        };
        write_classification(&run_dir, classification)?;
        if timeout_observation.state == DeviceState::DeviceAbsent {
            write_device_absent_recovery_plan(&run_dir, workspace)?;
        } else {
            write_next_experiment(
                &run_dir,
                "re-detect-transport: bounded Fastboot wait ended in a non-Fastboot state; preserve this run and return to source audit before another physical attempt",
            )?;
        }
        return Err(error);
    }
    let preboot_observation = observe_host(&args.serial)?;
    write_host_observation(&run_dir, "host-state-before-build", &preboot_observation)?;
    if preboot_observation.state != DeviceState::FastbootAvailable {
        write_classification(&run_dir, preboot_observation.state.as_str())?;
        if preboot_observation.state == DeviceState::DeviceAbsent {
            write_device_absent_recovery_plan(&run_dir, workspace)?;
        } else {
            write_next_experiment(
                &run_dir,
                "re-detect-transport: Fastboot disappeared before build; preserve this run and do not issue a device-side command",
            )?;
        }
        return Err(io::Error::other(format!(
            "Fastboot did not become available after bounded wait; state={}; logs: {}",
            preboot_observation.state.as_str(),
            run_dir.display()
        )));
    }
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
    let _usbmon = if args.usbmon {
        Some(UsbmonGuard::start(&run_dir)?)
    } else {
        None
    };
    let build = build_command(workspace, &args, &output);
    record_command_spec(&run_dir, "build", &build)?;
    record_fullerene_environment(&run_dir, &build)?;
    let build_output = run_capture(&run_dir.join("build.log"), &mut build_command_owned(build))?;
    if !build_output.status.success() || !output.is_file() {
        journal.save_final();
        write_classification(&run_dir, "build-or-audit-failure")?;
        return Err(io::Error::other("build/audit failed"));
    }
    let sha = sha256(&output)?;
    fs::write(
        run_dir.join("artifact.sha256"),
        format!("{sha}  {}\n", output.display()),
    )?;
    if let Some(expected_sha256) = expected_sha256 {
        fs::write(
            run_dir.join("expected-artifact.sha256"),
            format!("{expected_sha256}  {}\n", output.display()),
        )?;
        if sha != expected_sha256 {
            journal.save_final();
            write_classification(&run_dir, "artifact-sha256-mismatch")?;
            return Err(io::Error::other(format!(
                "candidate artifact SHA mismatch: expected {expected_sha256}, observed {sha}; no fastboot boot issued"
            )));
        }
    }

    let boot = boot_command(workspace, &output);
    record_command_spec(&run_dir, "boot", &boot)?;
    let before_boot = observe_host(&args.serial)?;
    write_host_observation(&run_dir, "host-state-before-boot", &before_boot)?;
    if before_boot.state != DeviceState::FastbootAvailable {
        journal.save_final();
        write_classification(&run_dir, before_boot.state.as_str())?;
        return Err(io::Error::other(format!(
            "Fastboot disappeared before fastboot boot; state={}; logs: {}",
            before_boot.state.as_str(),
            run_dir.display()
        )));
    }
    let boot_started = Instant::now();
    let boot_output = run_capture(&run_dir.join("boot.log"), &mut build_command_owned(boot))?;
    if !boot_output.status.success() {
        journal.save_final();
        write_classification(&run_dir, "fastboot-boot-command-failed")?;
        return Err(io::Error::other("Fastboot boot failed"));
    }

    wait_until_absent(BOOTLOADER_USB, 15);
    let deadline = Instant::now() + Duration::from_secs(args.enum_timeout);
    let mut android_fallback = false;
    let mut timeline = File::create(run_dir.join("lsusb-timeline.txt"))?;
    while Instant::now() < deadline {
        let stamp =
            Instant::now().duration_since(deadline - Duration::from_secs(args.enum_timeout));
        let observation = observe_host(&args.serial)?;
        append_host_timeline(&mut timeline, stamp, &observation)?;
        if observation.fullerene_usb {
            println!("Fullerene USB VID:PID appeared; verifying Fullerene-owned descriptors");
            let descriptor =
                capture_simple(&run_dir, "lsusb-v", "lsusb", &["-d", FULLERENE_USB, "-v"])?;
            if !fullerene_descriptor_is_self_identifying(&descriptor) {
                journal.save_final();
                write_host_observation(&run_dir, "host-state-fullerene", &observation)?;
                let classification = if descriptor.status.success() {
                    "fullerene-usb-present-non-fullerene-descriptor"
                } else {
                    "fullerene-usb-present-descriptor-read-failure"
                };
                write_classification(&run_dir, classification)?;
                return Err(io::Error::other(
                    "1234:0001 appeared without the expected Fullerene-owned descriptors",
                ));
            }
            println!("Fullerene USB enumeration and descriptor identity: PASS");
            write_host_observation(&run_dir, "host-state-fullerene", &observation)?;
            let _ = capture_simple(&run_dir, "lsusb-tree", "lsusb", &["-t"]);
            if args.super_speed && !has_superspeed_link(&run_dir)? {
                journal.save_final();
                write_classification(
                    &run_dir,
                    "fullerene-usb-present-without-required-superspeed",
                )?;
                return Err(io::Error::other("Fullerene USB has no SuperSpeed link"));
            }
            let hold_deadline = Instant::now() + Duration::from_secs(args.hold);
            while Instant::now() < hold_deadline {
                if !usb_present(FULLERENE_USB) {
                    journal.save_final();
                    write_classification(&run_dir, "fullerene-usb-disappeared-during-hold")?;
                    return Err(io::Error::other("Fullerene USB disappeared during hold"));
                }
                thread::sleep(Duration::from_secs(1));
            }
            journal.save_final();
            write_classification(&run_dir, "fullerene-usb-1234:0001-descriptor-read-success")?;
            println!("Fullerene USB handoff and hold verification: PASS");
            return Ok(());
        }
        if observation.android_usb {
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
        write_host_observation(
            &run_dir,
            "host-state-android-fallback",
            &observe_host(&args.serial)?,
        )?;
    } else {
        println!(
            "Fullerene USB did not enumerate; waiting up to {RECOVERY_TIMEOUT_SECS}s for probe recovery"
        );
        let recovery_deadline = Instant::now() + Duration::from_secs(RECOVERY_TIMEOUT_SECS);
        while Instant::now() < recovery_deadline {
            let observation = observe_host(&args.serial)?;
            let stamp = boot_started.elapsed();
            append_host_timeline(&mut timeline, stamp, &observation)?;
            if observation.android_usb {
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
                write_host_observation(&run_dir, "host-state-android-fallback", &observation)?;
                break;
            }
            if observation.bootloader_usb {
                if let Ok(output) = fastboot_command(&args.serial, &["getvar", "all"]).output() {
                    let mut file = File::create(run_dir.join("fastboot-getvar-after.txt"))?;
                    file.write_all(&output.stdout)?;
                    file.write_all(&output.stderr)?;
                }
                journal.save_final();
                write_host_observation(&run_dir, "host-state-fastboot-fallback", &observation)?;
                write_classification(&run_dir, "fastboot-fallback")?;
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
    journal.save_final();
    let final_observation = observe_host(&args.serial)?;
    write_host_observation(&run_dir, "host-state-final", &final_observation)?;
    let kernel_log = fs::read_to_string(run_dir.join("kernel-final.log")).ok();
    let classification = classify_postboot_result(&final_observation, kernel_log.as_deref(), None);
    write_classification(&run_dir, classification)?;
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
    println!("android-init={}", args.android_init);
    println!("android-init-ufs-execute={}", args.android_init_ufs_execute);
    println!("early-usb-handoff={}", args.early_usb_handoff);
    println!(
        "early-usb-before-dtb-scan={}",
        args.early_usb_before_dtb_scan
    );
    println!("entry-secure-wdt={}", args.entry_secure_wdt);
    println!("adb-return={}", args.adb_return);
    if let Some(route) = args.irq_route {
        println!("irq-route={}", route.as_str());
    }
    println!(
        "adb-reboot-to-fastboot={}",
        if adb_reboot_to_fastboot_enabled(
            args.adb_reboot_to_fastboot,
            args.no_adb_reboot_to_fastboot,
        ) {
            "enabled"
        } else {
            "disabled"
        }
    );
    if adb_reboot_to_fastboot_enabled(args.adb_reboot_to_fastboot, args.no_adb_reboot_to_fastboot) {
        println!("operation=adb reboot bootloader + fastboot boot only");
    } else {
        println!("operation=fastboot boot only");
    }
}

fn adb_reboot_to_fastboot_enabled(explicit: bool, disabled: bool) -> bool {
    explicit || !disabled
}

fn mode_name(args: &LoopArgs) -> &'static str {
    if args.android_init {
        if args.normal {
            "android-init-normal"
        } else if args.direct_handoff {
            "android-init-usb-gadget-handoff-direct"
        } else if args.bare_pullup {
            "android-init-usb-bare-pullup-probe"
        } else if args.pullup_only {
            "android-init-usb-pullup-probe"
        } else if args.super_speed {
            "android-init-usb-gadget-handoff-super-speed-probe"
        } else {
            "android-init-usb-gadget-handoff-probe"
        }
    } else if args.normal {
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
        if let Some(lane) = &args.qmp_lane {
            arguments.push("--usb-qmp-lane".to_owned());
            arguments.push(lane.clone());
        }
        if args.xbl_qmp_table {
            arguments.push("--usb-xbl-qmp-table".to_owned());
        }
        if args.xbl_hs_phy_table {
            arguments.push("--usb-xbl-hs-phy-table".to_owned());
        }
        if let Some(phase) = args.qmp_phase_stop {
            arguments.push("--usb-qmp-phase-stop".to_owned());
            arguments.push(phase.to_string());
        }
    }
    if args.android_init {
        arguments.push("--android-init".to_owned());
    }
    if args.adb_return {
        arguments.push("--adb-return".to_owned());
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
    if args.dma_cache_maintenance {
        arguments.push("--usb-gadget-handoff-dma-cache-maintenance".to_owned());
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
    if args.gadget_start_defaults_at_runstop {
        arguments.push("--usb-gadget-handoff-start-defaults-at-runstop".to_owned());
    }
    if args.min_runstop_delay {
        arguments.push("--usb-gadget-handoff-min-runstop-delay".to_owned());
    }
    if args.clock_branches_rearm {
        arguments.push("--usb-gadget-handoff-clock-branches-rearm".to_owned());
    }
    if args.usb_core_hs_clock {
        arguments.push("--usb-gadget-handoff-core-hs-clock".to_owned());
    }
    if args.usb2_full_core_reset {
        arguments.push("--usb-gadget-handoff-usb2-full-core-reset".to_owned());
    }
    if let Some(delay_us) = args.clock_stable_delay_us {
        arguments.push("--usb-gadget-handoff-clock-stable-delay-us".to_owned());
        arguments.push(delay_us.to_string());
    }
    if args.android_block_reset {
        arguments.push("--usb-gadget-handoff-android-block-reset".to_owned());
    }
    if args.refresh_hsphy_power {
        arguments.push("--usb-gadget-handoff-refresh-hsphy-power".to_owned());
    }
    if args.hsphy_program_vdda_voltage {
        arguments.push("--usb-gadget-handoff-hsphy-program-vdda-voltage".to_owned());
    }
    if args.hsphy_all_regulator_sets {
        arguments.push("--usb-gadget-handoff-hsphy-all-regulator-sets".to_owned());
    }
    if args.skip_usb2_phy_reset {
        arguments.push("--usb-gadget-handoff-skip-usb2-phy-reset".to_owned());
    }
    if args.event_ring_size_4096 {
        arguments.push("--usb-gadget-handoff-event-ring-size-4096".to_owned());
    }
    if args.start_after_connect {
        arguments.push("--usb-gadget-handoff-start-after-connect".to_owned());
    }
    if args.xbl_deferred_setup {
        arguments.push("--usb-gadget-handoff-xbl-deferred-setup".to_owned());
    }
    if args.xbl_ep0_in_data {
        arguments.push("--usb-gadget-handoff-xbl-ep0-in-data".to_owned());
    }
    if args.xbl_event_dma {
        arguments.push("--usb-gadget-handoff-xbl-event-dma".to_owned());
    }
    if args.xbl_ep0_config {
        arguments.push("--usb-gadget-handoff-xbl-ep0-config".to_owned());
    }
    if args.xbl_between_ep0 {
        arguments.push("--usb-gadget-handoff-xbl-between-ep0".to_owned());
    }
    if args.xbl_post_endpoint_global {
        arguments.push("--usb-gadget-handoff-xbl-post-endpoint-global".to_owned());
    }
    if args.xbl_stock_ep0_dma {
        arguments.push("--usb-gadget-handoff-xbl-stock-ep0-dma".to_owned());
    }
    if args.xbl_raw_runstop {
        arguments.push("--usb-gadget-handoff-xbl-raw-runstop".to_owned());
    }
    if args.source_exact_runstop {
        arguments.push("--usb-gadget-handoff-source-exact-runstop".to_owned());
    }
    if args.ss_reassert_runstop {
        arguments.push("--usb-gadget-handoff-ss-reassert-runstop".to_owned());
    }
    if args.ss_hold_runstop {
        arguments.push("--usb-gadget-handoff-ss-hold-runstop".to_owned());
    }
    if args.ss_retry_setup {
        arguments.push("--usb-gadget-handoff-ss-retry-setup".to_owned());
    }
    if args.ss_eager_setup {
        arguments.push("--usb-gadget-handoff-ss-eager-setup".to_owned());
    }
    if args.ss_source_susphy {
        arguments.push("--usb-gadget-handoff-ss-source-susphy".to_owned());
    }
    if args.ss_conndone_clear_hird {
        arguments.push("--usb-gadget-handoff-ss-conndone-clear-hird".to_owned());
    }
    if args.dt_hird_threshold {
        arguments.push("--usb-gadget-handoff-dt-hird-threshold".to_owned());
    }
    if args.android_hs_lpm {
        arguments.push("--usb-gadget-handoff-android-hs-lpm".to_owned());
    }
    if args.android_lpm_errata {
        arguments.push("--usb-gadget-handoff-android-lpm-errata".to_owned());
    }
    if args.abl_shared_hs_phy {
        arguments.push("--usb-gadget-handoff-abl-shared-hs-phy".to_owned());
    }
    if args.abl_devten {
        arguments.push("--usb-gadget-handoff-abl-devten".to_owned());
    }
    if args.abl_ep_config {
        arguments.push("--usb-gadget-handoff-abl-ep-config".to_owned());
    }
    if args.abl_command_params {
        arguments.push("--usb-gadget-handoff-abl-command-params".to_owned());
    }
    if args.abl_trb_flags {
        arguments.push("--usb-gadget-handoff-abl-trb-flags".to_owned());
    }
    if args.abl_setup_trb_buffer {
        arguments.push("--usb-gadget-handoff-abl-setup-trb-buffer".to_owned());
    }
    if args.abl_event_consume {
        arguments.push("--usb-gadget-handoff-abl-event-consume".to_owned());
    }
    if args.xbl_direction_trb {
        arguments.push("--usb-gadget-handoff-xbl-direction-trb".to_owned());
    }
    if args.xbl_trb_chain {
        arguments.push("--usb-gadget-handoff-xbl-trb-chain".to_owned());
    }
    if args.start_ungated {
        arguments.push("--usb-gadget-handoff-start-ungated".to_owned());
    }
    if args.event_ring_at_runstop {
        arguments.push("--usb-gadget-handoff-event-ring-at-runstop".to_owned());
    }
    if args.gadget_restart_at_runstop {
        arguments.push("--usb-gadget-handoff-gadget-restart-at-runstop".to_owned());
    }
    if args.gadget_start_only_at_runstop {
        arguments.push("--usb-gadget-handoff-gadget-start-only-at-runstop".to_owned());
    }
    if args.clear_gsi_after_reset {
        arguments.push("--usb-gadget-handoff-clear-gsi-after-reset".to_owned());
    }
    if args.hsphy_source_exact {
        arguments.push("--usb-gadget-handoff-hsphy-source-exact".to_owned());
    }
    if args.hsphy_xbl_exact {
        arguments.push("--usb-gadget-handoff-hsphy-xbl-exact".to_owned());
    }
    if args.hsphy_legacy_fallback {
        arguments.push("--usb-gadget-handoff-hsphy-legacy-fallback".to_owned());
    }
    if args.hsphy_before_reset {
        arguments.push("--usb-gadget-handoff-hsphy-before-reset".to_owned());
    }
    if args.hsphy_restore_suspend_n_after_runstop {
        arguments.push("--usb-gadget-handoff-hsphy-restore-suspend-n-after-runstop".to_owned());
    }
    if args.hsphy_restore_suspend_n_selected_after_runstop {
        arguments
            .push("--usb-gadget-handoff-hsphy-restore-suspend-n-selected-after-runstop".to_owned());
    }
    if args.ep0_initial_512 {
        arguments.push("--usb-gadget-handoff-ep0-initial-512".to_owned());
    }
    if args.dcfg_superspeed {
        arguments.push("--usb-gadget-handoff-dcfg-superspeed".to_owned());
    }
    if args.dcfg_fullspeed {
        arguments.push("--usb-gadget-handoff-dcfg-fullspeed".to_owned());
    }
    if args.dcfg_lowspeed {
        arguments.push("--usb-gadget-handoff-dcfg-lowspeed".to_owned());
    }
    if args.no_ss_vbus {
        arguments.push("--usb-gadget-handoff-no-ss-vbus".to_owned());
    }
    if args.usb2_core_reset_at_runstop {
        arguments.push("--usb-gadget-handoff-usb2-core-reset-at-runstop".to_owned());
    }
    if args.usb2_source_exact_device_reset {
        arguments.push("--usb-gadget-handoff-usb2-source-exact-device-reset".to_owned());
    }
    if args.usb2_qpr1_utmi_post_reset_only {
        arguments.push("--usb-gadget-handoff-usb2-qpr1-utmi-post-reset-only".to_owned());
    }
    if args.usb2_preserve_phy_interface {
        arguments.push("--usb-gadget-handoff-usb2-preserve-phy-interface".to_owned());
    }
    if args.ss_reassert_device_mode {
        arguments.push("--usb-gadget-handoff-ss-reassert-device-mode".to_owned());
    }
    if args.ss_reassert_core_clocks {
        arguments.push("--usb-gadget-handoff-ss-reassert-core-clocks".to_owned());
    }
    if args.ss_reassert_core_clocks_after_runstop {
        arguments.push("--usb-gadget-handoff-ss-reassert-core-clocks-after-runstop".to_owned());
    }
    if args.ss_reassert_domain_after_runstop {
        arguments.push("--usb-gadget-handoff-ss-reassert-domain-after-runstop".to_owned());
    }
    if args.ss_reassert_link_clocks_after_runstop {
        arguments.push("--usb-gadget-handoff-ss-reassert-link-clocks-after-runstop".to_owned());
    }
    if args.ss_android_dbm_reset {
        arguments.push("--usb-gadget-handoff-ss-android-dbm-reset".to_owned());
    }
    if args.ss_reassert_qmp_power {
        arguments.push("--usb-gadget-handoff-ss-reassert-qmp-power".to_owned());
    }
    if args.ss_reassert_qmp_power_after_gctl {
        arguments.push("--usb-gadget-handoff-ss-reassert-qmp-power-after-gctl".to_owned());
    }
    if args.ss_reinit_hs_phy {
        arguments.push("--usb-gadget-handoff-ss-reinit-hs-phy".to_owned());
    }
    if args.ss_pre_qmp_phy_setup {
        arguments.push("--usb-gadget-handoff-ss-pre-qmp-phy-setup".to_owned());
    }
    if args.ss_clear_qmp_autonomous {
        arguments.push("--usb-gadget-handoff-ss-clear-qmp-autonomous".to_owned());
    }
    if args.ss_reassert_qmp_clocks {
        arguments.push("--usb-gadget-handoff-ss-reassert-qmp-clocks".to_owned());
    }
    if args.ss_reassert_qmp_clocks_after_gctl {
        arguments.push("--usb-gadget-handoff-ss-reassert-qmp-clocks-after-gctl".to_owned());
    }
    if args.ss_reassert_hs_phy_ref_after_gctl {
        arguments.push("--usb-gadget-handoff-ss-reassert-hs-phy-ref-after-gctl".to_owned());
    }
    if args.ss_dis_sleep_mode_before_gadget {
        arguments.push("--usb-gadget-handoff-ss-dis-sleep-mode-before-gadget".to_owned());
    }
    if args.ss_clear_qmp_autonomous_exact {
        arguments.push("--usb-gadget-handoff-ss-clear-qmp-autonomous-exact".to_owned());
    }
    if args.ss_qmp_resume_wmb {
        arguments.push("--usb-gadget-handoff-ss-qmp-resume-wmb".to_owned());
    }
    if args.ss_qmp_lfps_clear_wmb {
        arguments.push("--usb-gadget-handoff-ss-qmp-lfps-clear-wmb".to_owned());
    }
    if args.ss_qmp_notify_disconnect {
        arguments.push("--usb-gadget-handoff-ss-qmp-notify-disconnect".to_owned());
    }
    if args.ss_clear_vbus_override_before_qmp {
        arguments.push("--usb-gadget-handoff-ss-clear-vbus-override-before-qmp".to_owned());
    }
    if args.ss_clear_keep_connect_before_stop {
        arguments.push("--usb-gadget-handoff-ss-clear-keep-connect-before-stop".to_owned());
    }
    if args.ss_clear_usb3_susphy_before_qmp {
        arguments.push("--usb-gadget-handoff-ss-clear-usb3-susphy-before-qmp".to_owned());
    }
    if args.ss_clear_usb3_susphy_before_runstop {
        arguments.push("--usb-gadget-handoff-ss-clear-usb3-susphy-before-runstop".to_owned());
    }
    if args.ss_clear_usb3_susphy_after_runstop {
        arguments.push("--usb-gadget-handoff-ss-clear-usb3-susphy-after-runstop".to_owned());
    }
    if args.ss_core_reset_at_runstop {
        arguments.push("--usb-gadget-handoff-ss-core-reset-at-runstop".to_owned());
    }
    if args.ss_separate_setup_buffer {
        arguments.push("--usb-gadget-handoff-ss-separate-setup-buffer".to_owned());
    }
    if args.ss_disable_gadget_irq_before_stop {
        arguments.push("--usb-gadget-handoff-ss-disable-gadget-irq-before-stop".to_owned());
    }
    if args.ss_disable_ep0_before_stop {
        arguments.push("--usb-gadget-handoff-ss-disable-ep0-before-stop".to_owned());
    }
    if args.ss_clear_gsi_stop_state {
        arguments.push("--usb-gadget-handoff-ss-clear-gsi-stop-state".to_owned());
    }
    if args.ss_lfps_timer {
        arguments.push("--usb-gadget-handoff-ss-lfps-timer".to_owned());
    }
    if args.ss_clear_ux_exit_px {
        arguments.push("--usb-gadget-handoff-ss-clear-ux-exit-px".to_owned());
    }
    if args.ss_preserve_ref_clock_state {
        arguments.push("--usb-gadget-handoff-ss-preserve-ref-clock-state".to_owned());
    }
    if args.ss_preserve_phy_state {
        arguments.push("--usb-gadget-handoff-ss-preserve-phy-state".to_owned());
    }
    if args.dcfg_ignstrmpp {
        arguments.push("--usb-gadget-handoff-dcfg-ignstrmpp".to_owned());
    }
    if args.usb2_susphy {
        arguments.push("--usb-gadget-handoff-usb2-susphy".to_owned());
    }
    if args.usb2_source_susphy {
        arguments.push("--usb-gadget-handoff-usb2-source-susphy".to_owned());
    }
    if args.usb2_source_exact_devten {
        arguments.push("--usb-gadget-handoff-usb2-source-exact-devten".to_owned());
    }
    if args.usb2_source_devten_before_runstop {
        arguments.push("--usb-gadget-handoff-usb2-source-devten-before-runstop".to_owned());
    }
    if args.usb2_source_exact_cmd_guard {
        arguments.push("--usb-gadget-handoff-usb2-cmd-guard".to_owned());
    }
    if args.usb2_source_exact_runstop {
        arguments.push("--usb-gadget-handoff-usb2-source-exact-runstop".to_owned());
    }
    if args.usb2_source_phy_setup {
        arguments.push("--usb-gadget-handoff-usb2-source-phy-setup".to_owned());
    }
    if args.usb2_dis_sleep_mode {
        arguments.push("--usb-gadget-handoff-usb2-dis-sleep-mode".to_owned());
    }
    if args.usb2_android_dbm_reset {
        arguments.push("--usb-gadget-handoff-usb2-android-dbm-reset".to_owned());
    }
    if args.ep0_stall_flush {
        arguments.push("--usb-gadget-handoff-ep0-stall-flush".to_owned());
    }
    if args.ep0_short_first_desc {
        arguments.push("--usb-gadget-handoff-ep0-short-first-desc".to_owned());
    }
    if args.ep0_txfifo_fix {
        arguments.push("--usb-gadget-handoff-ep0-txfifo-fix".to_owned());
    }
    if args.u2_freeclk_clear {
        arguments.push("--usb-gadget-handoff-u2-freeclk-clear".to_owned());
    }
    if args.u2_freeclk_set {
        arguments.push("--usb-gadget-handoff-u2-freeclk-set".to_owned());
    }
    if args.reset_resource {
        arguments.push("--usb-gadget-handoff-reset-resource".to_owned());
    }
    if args.reset_endpoints {
        arguments.push("--usb-gadget-handoff-reset-endpoints".to_owned());
    }
    if args.ep0_reset_clear_stall {
        arguments.push("--usb-gadget-handoff-ep0-reset-clear-stall".to_owned());
    }
    if args.ep0_reset_clear_test_mode {
        arguments.push("--usb-gadget-handoff-ep0-reset-clear-test-mode".to_owned());
    }
    if args.ep0_reset_callback_first {
        arguments.push("--usb-gadget-handoff-ep0-reset-callback-first".to_owned());
    }
    if args.ep0_reset_android_state_order {
        arguments.push("--usb-gadget-handoff-ep0-reset-android-state-order".to_owned());
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
    if args.signal_dma_post_runstop {
        arguments.push("--usb-signal-dma-post-runstop".to_owned());
    }
    if args.smmu_install_all {
        arguments.push("--usb-smmu-install-all".to_owned());
    }
    if let Some(mode) = args.signal_fsr_gate {
        arguments.push("--usb-signal-fsr-gate".to_owned());
        arguments.push(mode.to_string());
    }
    if let Some(mode) = args.signal_prev_trace_gate {
        arguments.push("--usb-signal-prev-trace-gate".to_owned());
        arguments.push(mode.to_string());
    }
    if let Some(phase) = args.signal_prev_qmp_gate {
        arguments.push("--usb-signal-prev-qmp-gate".to_owned());
        arguments.push(phase.to_string());
    }
    if args.signal_ram_gate {
        arguments.push("--usb-signal-ram-gate".to_owned());
    }
    if args.skip_typec_spmi {
        arguments.push("--usb-skip-typec-spmi".to_owned());
    }
    if args.u0_arm_probe {
        arguments.push("--usb-u0-arm-probe".to_owned());
    }
    if args.u0_arm_stop_first {
        arguments.push("--usb-u0-arm-stop-first".to_owned());
    }
    if args.wdt_bite_control {
        arguments.push("--usb-wdt-bite-control".to_owned());
    }
    if let Some(value) = &args.swdd_fnid {
        arguments.push("--usb-swdd-fnid".to_owned());
        arguments.push(value.clone());
    }
    if args.swdd_skip {
        arguments.push("--usb-swdd-skip".to_owned());
    }
    if args.arm_blip {
        arguments.push("--usb-arm-blip".to_owned());
    }
    if let Some(secs) = args.abs_reset_secs {
        arguments.push("--usb-abs-reset-secs".to_owned());
        arguments.push(secs.to_string());
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
    if let Some(value) = &args.utmi_preconnect_readout {
        arguments.push("--usb-utmi-preconnect-readout".to_owned());
        arguments.push(value.clone());
    }
    if let Some(value) = &args.utmi_postrun_readout {
        arguments.push("--usb-utmi-postrun-readout".to_owned());
        arguments.push(value.clone());
    }
    if let Some(value) = &args.pon_readout {
        arguments.push("--usb-pon-readout".to_owned());
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
    // Keep this explicitly selected secure-ownership A/B in the child build
    // environment. `command_from_spec()` removes inherited FULLERENE_* vars
    // so a direct harness export must be copied into the CommandSpec or the
    // physical image silently becomes the baseline artifact.
    if let Ok(value) = std::env::var("FULLERENE_AARCH64_USB_DISABLE_EUD") {
        envs.push(("FULLERENE_AARCH64_USB_DISABLE_EUD".to_owned(), value));
    }
    if args.android_init {
        envs.push((
            "FULLERENE_AARCH64_UFS_EXECUTE".to_owned(),
            if args.android_init_ufs_execute {
                "1"
            } else {
                "0"
            }
            .to_owned(),
        ));
    }
    if args.early_usb_handoff {
        envs.push((
            "FULLERENE_AARCH64_USB_EARLY_HANDOFF".to_owned(),
            "1".to_owned(),
        ));
    }
    if args.early_usb_before_dtb_scan {
        envs.push((
            "FULLERENE_AARCH64_USB_EARLY_BEFORE_DTB_SCAN".to_owned(),
            "1".to_owned(),
        ));
    }
    if args.entry_secure_wdt {
        envs.push((
            "FULLERENE_AARCH64_ENTRY_SECURE_WDT".to_owned(),
            "1".to_owned(),
        ));
    }
    if let Some(route) = args.irq_route {
        envs.push((
            "FULLERENE_AARCH64_USB_PROBE_IRQ_ROUTES".to_owned(),
            route.as_str().to_owned(),
        ));
    }
    if let Some(stage) = args.bare_pullup_stop_after {
        if stage < 4 {
            envs.push((
                "FULLERENE_AARCH64_USB_BARE_PULLUP_STOP_AFTER".to_owned(),
                stage.to_string(),
            ));
        }
    }
    if args.hyper_bare {
        envs.push((
            "FULLERENE_AARCH64_USB_HYPER_BARE".to_owned(),
            "1".to_owned(),
        ));
    }
    if args.no_core_reset {
        envs.push((
            "FULLERENE_AARCH64_USB_GADGET_HANDOFF_PRESERVE_CORE".to_owned(),
            "1".to_owned(),
        ));
    }
    if args.preserve_fastboot_runstop {
        envs.push((
            "FULLERENE_AARCH64_USB_GADGET_HANDOFF_PRESERVE_RUNSTOP".to_owned(),
            "1".to_owned(),
        ));
    }
    if args.usb2_susphy_after_runstop {
        envs.push((
            "FULLERENE_AARCH64_USB_USB2_SUSPHY_AFTER_RUNSTOP".to_owned(),
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
    // A harness process often inherits experimental FULLERENE_* variables
    // from the previous run. Remove them before applying the explicitly
    // selected values so an A/B really changes one declared variable.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("FULLERENE_") {
            command.env_remove(key);
        }
    }
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

fn fastboot_present(serial: &str) -> bool {
    Command::new("fastboot")
        .args(["devices", "-l"])
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .any(|line| line.split_whitespace().next() == Some(serial))
        })
        .unwrap_or(false)
}

fn wait_for_fastboot(serial: &str, timeout_secs: u64) -> io::Result<()> {
    if fastboot_present(serial) {
        return Ok(());
    }
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        if fastboot_present(serial) {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(1));
    }
    Err(io::Error::other(format!(
        "device {serial} is not available in Fastboot"
    )))
}

fn ensure_fastboot_from_adb(serial: &str, timeout_secs: u64, run_dir: &Path) -> io::Result<()> {
    if fastboot_present(serial) {
        fs::write(
            run_dir.join("transport-preflight.txt"),
            "Fastboot already present; adb reboot bootloader was not issued.\n",
        )?;
        return Ok(());
    }

    let state = capture_simple(
        run_dir,
        "adb-state-before-fastboot",
        "adb",
        &["-s", serial, "get-state"],
    )?;
    let state_text = String::from_utf8_lossy(&state.stdout).trim().to_owned();
    if !state.status.success() || state_text != "device" {
        let detail = String::from_utf8_lossy(&state.stderr).trim().to_owned();
        return Err(io::Error::other(format!(
            "device {serial} is neither in Fastboot nor ready in ADB (state={state_text:?}, detail={detail:?})"
        )));
    }

    let reboot = capture_simple(
        run_dir,
        "adb-reboot-bootloader",
        "adb",
        &["-s", serial, "reboot", "bootloader"],
    )?;
    if !reboot.status.success() {
        return Err(io::Error::other(format!(
            "adb reboot bootloader failed for device {serial}"
        )));
    }
    fs::write(
        run_dir.join("transport-preflight.txt"),
        format!(
            "ADB state was device; issued adb -s {serial} reboot bootloader; waiting for Fastboot.\n"
        ),
    )?;
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

fn fullerene_descriptor_text_is_self_identifying(text: &str) -> bool {
    let has_field_value = |field: &str, value: &str| {
        text.lines().any(|line| {
            let mut fields = line.split_whitespace();
            fields.next() == Some(field) && fields.next() == Some(value)
        })
    };
    let has_string = |field: &str, value: &str| {
        text.lines()
            .any(|line| line.trim_start().starts_with(field) && line.contains(value))
    };

    has_field_value("idVendor", "0x1234")
        && has_field_value("idProduct", "0x0001")
        && has_string("iManufacturer", "Fullerene")
        && has_string("iProduct", "Fullerene AArch64")
}

fn fullerene_descriptor_is_self_identifying(output: &Output) -> bool {
    if !output.status.success() {
        return false;
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    fullerene_descriptor_text_is_self_identifying(&text)
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

fn usb_field(line: &str, field: &str) -> Option<u32> {
    let marker = format!("{field} ");
    let start = line.find(&marker)? + marker.len();
    let digits = line[start..]
        .bytes()
        .take_while(u8::is_ascii_digit)
        .collect::<Vec<_>>();
    (!digits.is_empty()).then(|| {
        String::from_utf8_lossy(&digits)
            .parse()
            .expect("USB numeric field must fit in u32")
    })
}

fn tree_has_superspeed_link(tree: &str, bus: u32, device: u32) -> bool {
    let mut current_bus = None;
    tree.lines().any(|line| {
        if let Some(found_bus) = usb_field(line, "Bus") {
            current_bus = Some(found_bus);
        }
        current_bus == Some(bus)
            && usb_field(line, "Dev") == Some(device)
            && (line.contains("5000M") || line.contains("10000M"))
    })
}

fn has_superspeed_link(run_dir: &Path) -> io::Result<bool> {
    let listing = command_text("lsusb", &["-d", FULLERENE_USB])?;
    let bus = usb_field(&listing, "Bus").ok_or_else(|| {
        io::Error::other(format!("could not resolve the bus for {FULLERENE_USB}"))
    })?;
    let device = usb_field(&listing, "Device").ok_or_else(|| {
        io::Error::other(format!(
            "could not resolve the device address for {FULLERENE_USB}"
        ))
    })?;
    let tree = fs::read_to_string(run_dir.join("lsusb-tree.txt"))?;
    Ok(tree_has_superspeed_link(&tree, bus, device))
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

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn le_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

fn le_i32(bytes: &[u8]) -> i32 {
    i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn le_i64(bytes: &[u8]) -> i64 {
    i64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

fn is_device_descriptor_get(setup: &[u8]) -> bool {
    setup.len() >= 8
        && setup[0] == 0x80
        && setup[1] == 0x06
        && setup[2] == 0x00
        && setup[3] == 0x01
        && setup[4] == 0x00
        && setup[5] == 0x00
}

fn usbmon_summary(path: &Path) -> io::Result<String> {
    let bytes = fs::read(path)?;
    let mut offset = 0usize;
    let mut records = 0usize;
    let mut descriptor_ids = BTreeMap::new();
    let mut lines = vec![format!("capture_bytes={}", bytes.len())];
    while offset + 48 <= bytes.len() {
        let header = &bytes[offset..offset + 48];
        let id = le_u64(&header[0..8]);
        let event = header[8];
        let transfer_type = header[9];
        let endpoint = header[10];
        let device = header[11];
        let bus = le_u16(&header[12..14]);
        let seconds = le_i64(&header[16..24]);
        let micros = le_i32(&header[24..28]);
        let status = le_i32(&header[28..32]);
        let length = le_u32(&header[32..36]);
        let captured = le_u32(&header[36..40]) as usize;
        let record_size = 48usize.saturating_add(captured);
        if record_size < 48 || offset + record_size > bytes.len() {
            lines.push(format!(
                "truncated_record_offset={} captured={} remaining={}",
                offset,
                captured,
                bytes.len().saturating_sub(offset + 48)
            ));
            break;
        }
        let setup = &header[40..48];
        if transfer_type == 2 && event == b'S' && is_device_descriptor_get(setup) {
            descriptor_ids.insert((bus, id), (seconds, micros, device, endpoint));
            lines.push(format!(
                "descriptor_submit bus={bus} id=0x{id:016x} ts={seconds}.{micros:06} dev={} ep=0x{endpoint:02x} status={} length={} cap={}",
                device, status, length, captured
            ));
        } else if transfer_type == 2 && event == b'C' && device == 0 {
            if let Some((submit_seconds, submit_micros, submit_device, submit_endpoint)) =
                descriptor_ids.remove(&(bus, id))
            {
                lines.push(format!(
                    "descriptor_complete bus={bus} id=0x{id:016x} submit_ts={submit_seconds}.{submit_micros:06} ts={seconds}.{micros:06} dev={} ep=0x{endpoint:02x} status={} length={} cap={} submit_dev={} submit_ep=0x{submit_endpoint:02x}",
                    device, status, length, captured, submit_device
                ));
            }
        }
        offset += record_size;
        records += 1;
    }
    lines.insert(1, format!("records={} parsed_bytes={offset}", records));
    Ok(format!("{}\n", lines.join("\n")))
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
    use super::{
        CandidatesArgs, DeviceState, HostObservation, LoopArgs, MAX_CANDIDATE_RECOVERY_WAIT_SECS,
        TRACE_HEADER_BYTES, TRACE_MAGIC, TRACE_VERSION, adb_reboot_to_fastboot_enabled,
        adb_state_from_listing, build_command, classify_device_state, classify_postboot_result,
        experiment_manifest, fullerene_descriptor_text_is_self_identifying,
        kernel_log_has_non_android_attach, next_experiment_for_classification,
        normal_android_candidate_loop_args, parse_trace_header, tree_has_superspeed_link,
        usbmon_summary,
    };
    use std::{
        fs,
        path::{Path, PathBuf},
    };

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

    #[test]
    fn superspeed_check_is_scoped_to_the_requested_device() {
        let tree = "/: Bus 001.Port 1: Dev 1, Class=root_hub, 5000M\n\
                    |__ Port 1: Dev 7, If 0, Class=Vendor, 480M\n\
                    /: Bus 002.Port 1: Dev 1, Class=root_hub, 5000M\n\
                    |__ Port 1: Dev 7, If 0, Class=Vendor, 10000M\n";
        assert!(!tree_has_superspeed_link(tree, 1, 7));
        assert!(tree_has_superspeed_link(tree, 2, 7));
    }

    #[test]
    fn usbmon_summary_handles_reused_descriptor_urb_ids() {
        fn record(bus: u16, id: u64, event: u8, status: i32, setup: [u8; 8]) -> Vec<u8> {
            let mut header = vec![0u8; 48];
            header[0..8].copy_from_slice(&id.to_le_bytes());
            header[8] = event;
            header[9] = 2; // control transfer
            header[10] = 0;
            header[11] = 0; // address 0 during enumeration
            header[12..14].copy_from_slice(&bus.to_le_bytes());
            header[16..24].copy_from_slice(&1i64.to_le_bytes());
            header[24..28].copy_from_slice(&2i32.to_le_bytes());
            header[28..32].copy_from_slice(&status.to_le_bytes());
            header[32..36].copy_from_slice(&0u32.to_le_bytes());
            header[36..40].copy_from_slice(&0u32.to_le_bytes());
            header[40..48].copy_from_slice(&setup);
            header
        }

        let descriptor_get = [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x40, 0x00];
        let path = PathBuf::from(std::env::temp_dir()).join(format!(
            "fullerene-usbmon-summary-{}.bin",
            std::process::id()
        ));
        let mut capture = record(1, 0x41, b'S', -115, descriptor_get);
        capture.extend(record(2, 0x41, b'S', -115, descriptor_get));
        capture.extend(record(1, 0x41, b'C', -2, descriptor_get));
        capture.extend(record(2, 0x41, b'C', -71, [0; 8]));
        fs::write(&path, capture).unwrap();
        let summary = usbmon_summary(&path).unwrap();
        let _ = fs::remove_file(&path);

        assert_eq!(summary.matches("descriptor_submit ").count(), 2);
        assert_eq!(summary.matches("descriptor_complete ").count(), 2);
        assert!(summary.contains("descriptor_complete bus=1"));
        assert!(summary.contains("descriptor_complete bus=2"));
        assert!(summary.contains("status=-2"));
        assert!(summary.contains("status=-71"));
    }

    #[test]
    fn device_state_prioritizes_fullerene_then_fastboot_then_adb() {
        assert_eq!(
            classify_device_state(Some("device"), true, true, true),
            DeviceState::FullereneUsbAvailable
        );
        assert_eq!(
            classify_device_state(Some("device"), true, false, true),
            DeviceState::FastbootAvailable
        );
        assert_eq!(
            classify_device_state(Some("device"), false, false, false),
            DeviceState::AndroidAdbAvailable
        );
        assert_eq!(
            classify_device_state(Some("unauthorized"), false, false, true),
            DeviceState::UnknownUsbState
        );
        assert_eq!(
            classify_device_state(None, false, false, false),
            DeviceState::DeviceAbsent
        );
    }

    #[test]
    fn classification_selects_a_bounded_next_experiment() {
        assert!(
            next_experiment_for_classification("usb-attach-or-descriptor-failure--110")
                .contains("USB2 PHY RX/SOF")
        );
        assert!(next_experiment_for_classification("device-absent").contains("physically"));
        let absent_plan = next_experiment_for_classification("device-absent");
        assert!(absent_plan.contains("candidate-order=pre-dtb,post-dtb"));
        assert!(absent_plan.contains(
            "candidate-plan-command=cargo run -q -p flasks --bin bramble-usb -- candidates"
        ));
        assert!(absent_plan.contains("candidate-common-loop-flags=--android-init"));
        assert!(absent_plan.contains("candidate-profile-exclusions=--android-resource-order"));
        assert!(absent_plan.contains("allowed-device-operations=adb reboot bootloader"));
        assert!(absent_plan.contains("forbidden-device-operations=flash; erase; readback"));
        assert!(
            absent_plan.contains("candidate.pre-dtb.extra-loop-flag=--early-usb-before-dtb-scan")
        );
        assert!(absent_plan.contains("candidate.post-dtb.extra-loop-flag=none"));
        assert!(absent_plan.contains("candidate.pre-dtb.expected_sha256=bf72b5"));
        assert!(absent_plan.contains("candidate.post-dtb.expected_sha256=d0b8e4"));
        assert!(absent_plan.contains("device-operation-while-absent=none"));
        let mismatch_plan = next_experiment_for_classification("artifact-sha256-mismatch");
        assert!(mismatch_plan.contains("do not issue fastboot boot"));
        assert!(
            next_experiment_for_classification("fullerene-usb-1234:0001-descriptor-read-success")
                .starts_with("none:")
        );
    }

    #[test]
    fn candidate_plan_changes_only_dtb_ordering_between_profiles() {
        let args = CandidatesArgs::default();
        let pre = normal_android_candidate_loop_args(&args, true);
        let post = normal_android_candidate_loop_args(&args, false);
        assert!(pre.android_init);
        assert!(pre.adb_return);
        assert!(pre.early_usb_handoff);
        assert!(pre.entry_secure_wdt);
        assert!(pre.direct_handoff);
        assert!(pre.no_smmu);
        assert!(pre.dma_cache_maintenance);
        assert!(pre.early_usb_before_dtb_scan);
        assert!(!post.early_usb_before_dtb_scan);
        assert_eq!(pre.android_init_ufs_execute, post.android_init_ufs_execute);
        assert_eq!(
            pre.usb2_source_exact_runstop,
            post.usb2_source_exact_runstop
        );
    }

    #[test]
    fn candidate_recovery_wait_defaults_to_stop_and_is_bounded() {
        assert_eq!(CandidatesArgs::default().recovery_wait_secs, 0);
        assert_eq!(900_u64.min(MAX_CANDIDATE_RECOVERY_WAIT_SECS), 900);
        assert_eq!(901_u64.min(MAX_CANDIDATE_RECOVERY_WAIT_SECS), 900);
    }

    #[test]
    fn candidate_build_command_matches_the_reproduced_profile() {
        let args = CandidatesArgs::default();
        let pre = normal_android_candidate_loop_args(&args, true);
        let spec = build_command(
            Path::new("/workspace"),
            &pre,
            Path::new("/workspace/candidate.img"),
        );
        let required = [
            "--android-init",
            "--adb-return",
            "--usb-gadget-handoff-probe",
            "--usb-gadget-handoff-direct",
            "--usb-gadget-handoff-no-smmu",
            "--usb-gadget-handoff-dma-cache-maintenance",
            "--usb-gadget-handoff-start-after-connect",
            "--usb-gadget-handoff-refresh-hsphy-power",
            "--usb-gadget-handoff-hsphy-source-exact",
            "--usb-gadget-handoff-usb2-source-exact-device-reset",
            "--usb-gadget-handoff-usb2-source-susphy",
            "--usb-gadget-handoff-usb2-source-exact-devten",
            "--usb-gadget-handoff-usb2-source-devten-before-runstop",
            "--usb-gadget-handoff-usb2-cmd-guard",
            "--usb-gadget-handoff-usb2-source-exact-runstop",
        ];
        for flag in required {
            assert!(
                spec.arguments.iter().any(|argument| argument == flag),
                "missing {flag}"
            );
        }
        assert!(
            !spec
                .arguments
                .iter()
                .any(|argument| argument == "--usb-gadget-handoff-android-resource-order")
        );
        assert!(
            !spec
                .arguments
                .iter()
                .any(|argument| argument == "--usb-ep0-signal-probe")
        );
        assert!(
            spec.envs
                .contains(&("FULLERENE_AARCH64_UFS_EXECUTE".to_owned(), "0".to_owned()))
        );
        assert!(spec.envs.contains(&(
            "FULLERENE_AARCH64_USB_EARLY_HANDOFF".to_owned(),
            "1".to_owned()
        )));
        assert!(spec.envs.contains(&(
            "FULLERENE_AARCH64_USB_EARLY_BEFORE_DTB_SCAN".to_owned(),
            "1".to_owned()
        )));
        assert!(spec.envs.contains(&(
            "FULLERENE_AARCH64_ENTRY_SECURE_WDT".to_owned(),
            "1".to_owned()
        )));
    }

    #[test]
    fn adb_listing_parser_ignores_header_and_other_serials() {
        let listing =
            "List of devices attached\nother device\n26191JECB00076 unauthorized usb:1-9\n";
        assert_eq!(
            adb_state_from_listing("26191JECB00076", listing).as_deref(),
            Some("unauthorized")
        );
        assert_eq!(adb_state_from_listing("missing", listing), None);
    }

    #[test]
    fn adb_to_fastboot_transition_is_autonomous_by_default() {
        assert!(adb_reboot_to_fastboot_enabled(false, false));
        assert!(adb_reboot_to_fastboot_enabled(true, false));
        assert!(!adb_reboot_to_fastboot_enabled(false, true));
    }

    #[test]
    fn experiment_manifest_marks_combined_profiles_honestly() {
        let baseline = experiment_manifest(&LoopArgs::default());
        assert!(baseline.contains("changed_variable=baseline"));

        let mut profile = LoopArgs::default();
        profile.direct_handoff = true;
        profile.no_smmu = true;
        profile.dcfg_ignstrmpp = true;
        profile.signal_probe = true;
        profile.signal_early_drop = Some(1);
        let manifest = experiment_manifest(&profile);
        assert!(manifest.contains("changed_variable=combined-profile"));
        assert!(
            manifest
                .contains("direct-handoff=true,no-smmu=true,dcfg-ignstrmpp=true,signal-probe=true,signal-early-drop=1")
        );
    }

    #[test]
    fn experiment_manifest_includes_new_loop_fields_without_manual_listing() {
        let mut profile = LoopArgs::default();
        profile.utmi_preconnect_readout = Some("hsphy-status".to_owned());
        profile.enum_timeout = 61;
        let manifest = experiment_manifest(&profile);
        assert!(manifest.contains("utmi-preconnect-readout=hsphy-status"));
        assert!(manifest.contains("enum-timeout=61"));
        assert!(
            manifest.contains("experiment_id=enum-timeout=61,utmi-preconnect-readout=hsphy-status")
        );
    }

    #[test]
    fn android_init_build_spec_records_safe_storage_and_early_boundaries() {
        let mut profile = LoopArgs::default();
        profile.android_init = true;
        profile.early_usb_handoff = true;
        profile.early_usb_before_dtb_scan = true;
        profile.entry_secure_wdt = true;
        let spec = build_command(
            PathBuf::from("/workspace").as_path(),
            &profile,
            PathBuf::from("out.img").as_path(),
        );
        assert!(spec.arguments.iter().any(|arg| arg == "--android-init"));
        assert!(
            spec.envs
                .contains(&("FULLERENE_AARCH64_UFS_EXECUTE".to_owned(), "0".to_owned()))
        );
        assert!(spec.envs.contains(&(
            "FULLERENE_AARCH64_USB_EARLY_HANDOFF".to_owned(),
            "1".to_owned()
        )));
        assert!(spec.envs.contains(&(
            "FULLERENE_AARCH64_USB_EARLY_BEFORE_DTB_SCAN".to_owned(),
            "1".to_owned()
        )));
        assert!(spec.envs.contains(&(
            "FULLERENE_AARCH64_ENTRY_SECURE_WDT".to_owned(),
            "1".to_owned()
        )));

        profile.android_init_ufs_execute = true;
        let spec = build_command(
            PathBuf::from("/workspace").as_path(),
            &profile,
            PathBuf::from("out.img").as_path(),
        );
        assert!(
            spec.envs
                .contains(&("FULLERENE_AARCH64_UFS_EXECUTE".to_owned(), "1".to_owned()))
        );
    }

    #[test]
    fn fullerene_descriptor_requires_fullerene_owned_identity() {
        let valid = concat!(
            "idVendor           0x1234 Fullerene\n",
            "idProduct          0x0001\n",
            "iManufacturer           1 Fullerene\n",
            "iProduct                2 Fullerene AArch64\n",
        );
        assert!(fullerene_descriptor_text_is_self_identifying(valid));

        let android_configfs = concat!(
            "idVendor           0x1234\n",
            "idProduct          0x0001\n",
            "iManufacturer           1 Google\n",
            "iProduct                2 Android Gadget\n",
        );
        assert!(!fullerene_descriptor_text_is_self_identifying(
            android_configfs
        ));
    }

    #[test]
    fn postboot_classifier_distinguishes_descriptor_errors_from_absence() {
        let absent = HostObservation {
            state: DeviceState::DeviceAbsent,
            adb_state: None,
            adb_devices: String::new(),
            fastboot_devices: String::new(),
            lsusb: String::new(),
            fastboot: false,
            fullerene_usb: false,
            bootloader_usb: false,
            android_usb: false,
        };
        assert_eq!(
            classify_postboot_result(&absent, Some("device descriptor read/64, error -110"), None),
            "usb-attach-or-descriptor-failure--110"
        );
        assert_eq!(
            classify_postboot_result(&absent, None, None),
            "google-logo-or-software-unrecoverable-suspected"
        );
        let android = HostObservation {
            state: DeviceState::AndroidAdbAvailable,
            adb_state: Some("device".to_owned()),
            adb_devices: String::new(),
            fastboot_devices: String::new(),
            lsusb: String::new(),
            fastboot: false,
            fullerene_usb: false,
            bootloader_usb: false,
            android_usb: true,
        };
        assert_eq!(
            classify_postboot_result(
                &android,
                Some("new high-speed USB device; error -110"),
                None
            ),
            "usb-attach-or-descriptor-failure--110"
        );
    }

    #[test]
    fn postboot_classifier_ignores_android_superspeed_fallback_attach() {
        let android = HostObservation {
            state: DeviceState::AndroidAdbAvailable,
            adb_state: Some("device".to_owned()),
            adb_devices: String::new(),
            fastboot_devices: String::new(),
            lsusb: String::new(),
            fastboot: false,
            fullerene_usb: false,
            bootloader_usb: false,
            android_usb: true,
        };
        let android_log = concat!(
            "usb 2-1: new SuperSpeed USB device number 69 using xhci_hcd\n",
            "usb 2-1: New USB device found, idVendor=18d1,idProduct=4ee7\n",
        );
        assert!(!kernel_log_has_non_android_attach(android_log));
        assert_eq!(
            classify_postboot_result(&android, Some(android_log), None),
            "android-fallback"
        );

        let fullerene_log = concat!(
            "usb 1-9: new high-speed USB device number 35 using xhci_hcd\n",
            "usb 2-1: new SuperSpeed USB device number 69 using xhci_hcd\n",
            "usb 2-1: New USB device found, idVendor=18d1,idProduct=4ee7\n",
        );
        assert!(kernel_log_has_non_android_attach(fullerene_log));
        assert_eq!(
            classify_postboot_result(&android, Some(fullerene_log), None),
            "usb-attach-without-registered-descriptor"
        );
    }
}
