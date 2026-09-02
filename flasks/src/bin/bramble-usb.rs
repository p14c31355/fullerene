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
    /// Force QMP's USB lane A or B without changing PMIC Type-C role state.
    #[arg(long, value_parser = ["a", "b"])]
    qmp_lane: Option<String>,
    /// Stop immediately after a QMP phase marker (1..=8) and use same-boot
    /// USB2 attach presence as the reached/not-reached readout.
    #[arg(long, value_name = "PHASE", value_parser = clap::value_parser!(u32).range(1..=8))]
    qmp_phase_stop: Option<u32>,
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
    /// Reuse Fastboot's event-ring DMA page instead of the linker-reserved
    /// EP0 DMA objects. This is a Rust-only firmware-mapping differential.
    #[arg(long)]
    reuse_fastboot_dma: bool,
    #[arg(long)]
    no_transfer_resource: bool,
    #[arg(long)]
    android_resource_order: bool,
    /// Re-enable Android msm's iface/core/sleep controller branches before
    /// the UTMI branch at the direct USB2 handoff boundary.
    #[arg(long)]
    clock_branches_rearm: bool,
    /// Select Android msm's HS performance state for the DWC3 core clock
    /// (66.666667 MHz) at the direct USB2 handoff boundary.
    #[arg(long)]
    usb_core_hs_clock: bool,
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
    /// Use Bramble's DT HIRD threshold (0x10) instead of XBL's observed 7.
    #[arg(long)]
    dt_hird_threshold: bool,
    /// Apply Android msm's HS Connect Done LPM/HIRD controller policy.
    #[arg(long)]
    android_hs_lpm: bool,
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
    /// Reproduce Qualcomm's DWC3_CONTROLLER_NOTIFY_CLEAR_DB immediately
    /// after the device-core reset (A/B).
    #[arg(long)]
    clear_gsi_after_reset: bool,
    /// Use the source-exact Bramble msm_hsphy_init() sequence on the direct
    /// USB2 handoff instead of the legacy helper's local RTUNE/delay steps.
    #[arg(long)]
    hsphy_source_exact: bool,
    /// Start EP0 with the Linux/Android 512-byte descriptor state.
    #[arg(long)]
    ep0_initial_512: bool,
    /// Keep DCFG at the Bramble maximum-speed SuperSpeed state at Run/Stop.
    #[arg(long)]
    dcfg_superspeed: bool,
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
    /// Set DCFG.IGNSTRMPP in the direct gadget-start sequence (A/B).
    #[arg(long)]
    dcfg_ignstrmpp: bool,
    /// Restore USB2 SUSPHY immediately before the direct Run/Stop boundary.
    #[arg(long)]
    usb2_susphy: bool,
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
    /// Apply Android msm's reset callback, test-mode clear, and EP0 stall
    /// clear in source order while preserving the armed EP0 transfer.
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
    /// Previous-boot trace gate: 1 = attach only when the previous boot's
    /// retained trace reached a SETUP (progress code >= 2), 2 = attach only
    /// when it did not. A suppressed run resets before publishing the
    /// pull-up, so the kernel.log attach-line presence is the one-bit
    /// readout, immune to bootloader attach-time jitter.
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
    /// Emit one host-visible DCTL.SDIS blip after the post-Run/Stop arm
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
            enum_timeout: 30,
            hold: 30,
            fastboot_wait: 30,
            irq_route: None,
            super_speed: false,
            qmp_lane: None,
            qmp_phase_stop: None,
            normal: false,
            direct_handoff: false,
            pullup_only: false,
            bare_pullup: false,
            bare_pullup_stop_after: None,
            hyper_bare: false,
            stop_after_stage: None,
            no_smmu: false,
            reuse_fastboot_dma: false,
            no_transfer_resource: false,
            android_resource_order: false,
            clock_branches_rearm: false,
            usb_core_hs_clock: false,
            clock_stable_delay_us: None,
            android_block_reset: false,
            refresh_hsphy_power: false,
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
            dt_hird_threshold: false,
            android_hs_lpm: false,
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
            clear_gsi_after_reset: false,
            hsphy_source_exact: false,
            ep0_initial_512: false,
            dcfg_superspeed: false,
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
            ss_disable_gadget_irq_before_stop: false,
            ss_disable_ep0_before_stop: false,
            ss_clear_gsi_stop_state: false,
            ss_lfps_timer: false,
            ss_clear_ux_exit_px: false,
            ss_preserve_ref_clock_state: false,
            dcfg_ignstrmpp: false,
            usb2_susphy: false,
            ep0_stall_flush: false,
            ep0_short_first_desc: false,
            ep0_txfifo_fix: false,
            u2_freeclk_clear: false,
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
                if let Err(recovery) = wait_for_fastboot(&args.serial, args.fastboot_wait) {
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
        no_smmu: args.no_smmu,
        no_core_reset: args.no_core_reset,
        ..LoopArgs::default()
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
            "--normal cannot be combined with --super-speed, --pullup-only, --bare-pullup, --stop-after-stage, or --direct-handoff",
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
    if args.event_ring_at_runstop && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--event-ring-at-runstop requires --direct-handoff",
        ));
    }
    if args.gadget_restart_at_runstop && !args.direct_handoff && !args.super_speed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--gadget-restart-at-runstop requires --direct-handoff or --super-speed",
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
    if args.usb2_susphy && !args.direct_handoff {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--usb2-susphy requires --direct-handoff",
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
        && !matches!(args.signal_prev_trace_gate, Some(1 | 2 | 3))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--signal-prev-trace-gate must be 1, 2, or 3",
        ));
    }
    if args.signal_prev_qmp_gate.is_some()
        && !matches!(args.signal_prev_qmp_gate, Some(1..=8))
    {
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
    // iteration. Do not request a reboot here: the Bramble workflow permits
    // only `fastboot boot` as a device operation. Wait for the bootloader USB
    // identity to return, or require the operator to restore it outside this
    // harness.
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
        if let Some(lane) = &args.qmp_lane {
            arguments.push("--usb-qmp-lane".to_owned());
            arguments.push(lane.clone());
        }
        if let Some(phase) = args.qmp_phase_stop {
            arguments.push("--usb-qmp-phase-stop".to_owned());
            arguments.push(phase.to_string());
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
    if args.clock_branches_rearm {
        arguments.push("--usb-gadget-handoff-clock-branches-rearm".to_owned());
    }
    if args.usb_core_hs_clock {
        arguments.push("--usb-gadget-handoff-core-hs-clock".to_owned());
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
    if args.dt_hird_threshold {
        arguments.push("--usb-gadget-handoff-dt-hird-threshold".to_owned());
    }
    if args.android_hs_lpm {
        arguments.push("--usb-gadget-handoff-android-hs-lpm".to_owned());
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
    if args.clear_gsi_after_reset {
        arguments.push("--usb-gadget-handoff-clear-gsi-after-reset".to_owned());
    }
    if args.hsphy_source_exact {
        arguments.push("--usb-gadget-handoff-hsphy-source-exact".to_owned());
    }
    if args.ep0_initial_512 {
        arguments.push("--usb-gadget-handoff-ep0-initial-512".to_owned());
    }
    if args.dcfg_superspeed {
        arguments.push("--usb-gadget-handoff-dcfg-superspeed".to_owned());
    }
    if args.ss_reassert_device_mode {
        arguments.push("--usb-gadget-handoff-ss-reassert-device-mode".to_owned());
    }
    if args.ss_reassert_core_clocks {
        arguments.push("--usb-gadget-handoff-ss-reassert-core-clocks".to_owned());
    }
    if args.ss_reassert_core_clocks_after_runstop {
        arguments.push(
            "--usb-gadget-handoff-ss-reassert-core-clocks-after-runstop".to_owned(),
        );
    }
    if args.ss_reassert_domain_after_runstop {
        arguments.push("--usb-gadget-handoff-ss-reassert-domain-after-runstop".to_owned());
    }
    if args.ss_reassert_link_clocks_after_runstop {
        arguments.push(
            "--usb-gadget-handoff-ss-reassert-link-clocks-after-runstop".to_owned(),
        );
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
    if args.dcfg_ignstrmpp {
        arguments.push("--usb-gadget-handoff-dcfg-ignstrmpp".to_owned());
    }
    if args.usb2_susphy {
        arguments.push("--usb-gadget-handoff-usb2-susphy".to_owned());
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
        TRACE_HEADER_BYTES, TRACE_MAGIC, TRACE_VERSION, parse_trace_header,
        tree_has_superspeed_link,
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
}
