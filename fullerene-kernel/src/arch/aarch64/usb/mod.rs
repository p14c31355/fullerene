//! DWC3 device-mode support for the Bramble USB-C port.
//!
//! The early gadget has one bounded vendor function, while its controller
//! lifecycle follows the Qualcomm platform contract: Type-C attach,
//! PHY/session state, the Android event-buffer layout, SMMU DMA, GIC/PDC
//! interrupts, EP0 disconnect/reset/error handling, and ordinary UDC data
//! requests are kept separate from protocol data.
//! Early boot polls as a recovery path when firmware retains GIC ownership;
//! the same event ring is drained from the IRQ handler once the GIC is live.

use config::{
    DEVICE_MODE_WRITE_PROBE, apply_usb31_gadget_reference_deltas,
    configure_android_hs_connect_done_policy, configure_dwc3_device_mode,
    configure_dwc3_global_control, configure_gadget_speed, configure_gadget_start_defaults,
    configure_usb2_phy_interface, configure_usb31_lfps_exit_timer, configure_usb31_phy_setup,
    configure_usb31_phy_setup_pre_reset, enable_gadget_susphy, enable_usb2_gadget_susphy,
    qscratch_set, run_stop_value,
};
use control::{
    core_soft_reset, device_soft_reset, release_usb3_phy_reset, run_stop_device,
    run_stop_device_no_readback, stop_running_device, write_dctl_safe,
};
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use config::configure_usb2_phy_interface_pre_reset;
use log::{log_hex, log_hex_value, log_puts};
use mmio::*;
use phy::{
    init_hsphy, init_hsphy_source_exact, init_qmp_phy, qmp_set_autonomous_mode,
    select_utmi_pipe_clock, select_utmi_pipe_clock_post_reset, update_dwc3_ref_clock,
};
pub use phy::{qmp_phase_probe_reached, qmp_phase_probe_requested};
mod config;
mod control;
mod debug_transport;
mod endpoint_io;
mod ep0;
mod link_recovery;
mod poll;
mod readout;
mod runtime;
mod signals;
mod usb2_handoff;

// Keep the flat `usb::...` API while the implementations live in focused files.
use endpoint_io::*;
#[allow(unused_imports)]
pub use endpoint_io::*;
use ep0::*;
use link_recovery::*;
#[allow(unused_imports)]
pub use link_recovery::*;
use poll::*;
#[allow(unused_imports)]
pub use poll::*;
use readout::{capture_ss_state_snapshot, harvest_trace_outcome, usb2_live_word};
pub use readout::{
    diag_readout_code, dwc3_debug_window_queue, ep1_data_phase_complete, latch_snpsid,
    protocol_readout_code, rescue_read64, trace_dwc3_boundary, trace_dwc3_debug_sample,
    trace_dwc3_debug_stage, trace_dwc3_debug_window_sample, utmi_readout_code,
};
use runtime::*;
#[allow(unused_imports)]
pub use runtime::*;
use signals::*;
#[allow(unused_imports)]
pub use signals::*;
use usb2_handoff::*;
#[allow(unused_imports)]
pub use usb2_handoff::*;
#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
mod gadget_handoff;
mod phy;
mod phy_tables;
mod super_speed;
pub use phy_tables::hsphy_node_code;
pub use phy_tables::hsphy_table_source;
pub use phy_tables::install_dt_phy_sequences;
pub use phy_tables::{record_hs_dt_node_identity, record_hs_dt_param_override_observation};
use trace::{fill_trace_control_response, trace_begin, trace_event};
pub mod trace;
use trace::*;
pub use trace::{
    TRACE_BOOT_USB_ENTRY, TRACE_EXCEPTION_SYNC, TRACE_PLATFORM_IRQ, TRACE_PROBE_WATCHDOG,
    TRACE_TYPEC_BEGIN, TRACE_TYPEC_DONE, TRACE_TYPEC_EVENT, TRACE_UDC_REARM,
    TRACE_USB_HANDOFF_BEGIN, dump_trace, prev_boot_boundary_code, prev_boot_progress_code,
    prev_boot_qmp_phase_code, trace_head, trace_last_event, trace_marker, trace_probe_begin,
    trace_reset_head_for_boot,
};
mod log;
mod mmio;
mod smmu;
mod watchdog;

use smmu::{
    adopt_smmu_dma_mapping, service_smmu_fault, smmu_install_stream_bypass, smmu_stream_s2cr_type,
};
pub use smmu::{configure_dwc3_smmu, probe_smmu_stream_state};
use watchdog::{
    CURRENT_EL_AT_ENTRY, MDCR_EL2_AT_ENTRY, SWDD_AVAIL, SWDD_RESULT, SWDD_STD, WDT_KPSS_EN_AT_ENTRY,
};
pub use watchdog::{secure_wdt_disable, secure_wdt_probes, u0_arm_wdt_bite, wdt_pet};

use super::{
    uart,
    usb_protocol::{
        ControlAction, Ep0Simulator, GSI_DEFAULT_NUM_BUFFERS, GadgetDriver,
        TRACE_CONTROL_ENTRY_BYTES, TRACE_CONTROL_HEADER_BYTES, TRACE_CONTROL_PAGE_ENTRIES,
        TRACE_CONTROL_REQUEST, TRACE_CONTROL_REQUEST_TYPE, UsbUdc, gsi_ring_shape,
    },
    usb_regs::*,
};

/// Return whether a GSNPSID read has a known Synopsys USB core IP prefix.
///
/// DWC_usb3 uses the 0x5532/0x5533 prefixes, while Bramble's DWC_usb31
/// reports 0x3331 (and DWC_usb32 reports 0x3332).  Keep this predicate in one
/// place: treating a USB31 read as unknown makes read-only domain probes look
/// like a post-Run/Stop core failure even when the MMIO read is valid.
#[inline]
fn known_dwc_core_ip(snpsid: u32) -> bool {
    matches!(snpsid >> 16, 0x5532 | DWC3_IP | DWC31_IP | DWC32_IP)
}

/// Return qpr1's `dwc3_gadget_enable_irq()` device-event mask.
///
/// This is an opt-in source-order differential. The direct Bramble path
/// normally publishes a broader polling/debug mask, while qpr1 enables only
/// the controller events needed by the Android gadget driver (plus vendor,
/// overflow, command-complete, and erratic-error diagnostics). `ULSTCNGEN`
/// is present only on pre-2.30a cores, matching the source revision guard.
#[inline]
unsafe fn qpr1_gadget_devten() -> u32 {
    unsafe {
        let mut mask = DEVTEN_VENDOR_EVENT
            | DEVTEN_OVERFLOW
            | DEVTEN_CMD_COMPLETE
            | DEVTEN_ERRATIC_ERROR
            | DEVTEN_WAKEUP
            | DEVTEN_CONNECT_DONE
            | DEVTEN_USB_RESET
            | DEVTEN_DISCONNECT;
        let snpsid = read(GSNPSID);
        let revision = if matches!(snpsid >> 16, DWC31_IP | DWC32_IP) {
            read(VER_NUMBER) | DWC3_REVISION_IS_DWC31
        } else {
            snpsid
        };
        if revision < DWC3_REVISION_230A {
            mask |= DEVTEN_LINK_STATUS_CHANGE;
        }
        mask
    }
}

/// Select the device-event mask for the direct Fastboot-reuse handoff.
///
/// Keep this selection independent of the publication point: the deferred
/// Bramble profile publishes the same value only after its U0-guarded
/// STARTTRANSFER retry has actually armed EP0.
///
/// # Audit (2026-09-24): the unsourced "broad" default was retired
///
/// The default used to be a *broad* mask while the source-exact mask was
/// opt-in behind `..._usb2_source_exact_devten`. Auditing qpr1
/// `dwc3_gadget_enable_irq()` (`gadget.c:2324-2343`) against this tree settled
/// the question the refactor raised - the broad mask was not merely different,
/// it was wrong in two ways relative to the source:
///
/// * it enabled `SUSPEND`/`EOPF` (bit 6). The vendor deliberately omits the
///   end-of-periodic-frame bit from the gadget-start mask and adds it only
///   after Connect Done, on revisions >= 2.30a (see
///   `qpr1_enable_eopf_on_connect_done()` below).
/// * it omitted `VENDOR_EVENT` (bit 12), which the vendor does enable.
///
/// It also disagreed with *itself*: the four `write(DEVTEN, ...)` sites each
/// spelled out ten flags while this selector returned seven, so the effective
/// controller-event mask depended on which code path published last. The
/// vendor's value on this core is `0x1e17`, which an independent audit of the
/// Linux path also recorded (`DEVTEN` written before `DCTL.RUN_STOP`).
///
/// The source-exact mask is therefore now the default *and* the only value
/// published, and the four write sites call this selector instead of carrying
/// their own copies.
#[inline]
unsafe fn direct_gadget_devten() -> u32 {
    unsafe {
        if cfg!(any(
            fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup,
            fullerene_aarch64_usb_abl_devten
        )) {
            DEVTEN_DISCONNECT | DEVTEN_USB_RESET | DEVTEN_CONNECT_DONE | DEVTEN_SUSPEND
        } else {
            qpr1_gadget_devten()
        }
    }
}

/// Mirror qpr1's `dwc3_gadget_conndone_interrupt()` event-mask transition.
///
/// qpr1 deliberately omits the EOPF bit from the initial gadget-start mask
/// and adds it only after Connect Done on revisions >= 2.30a, where the bit is
/// reported as the suspend event. Keep this separate from
/// `qpr1_gadget_devten()` so the initial and post-Connect Done masks retain
/// the source ordering.
#[inline]
unsafe fn qpr1_enable_eopf_on_connect_done() {
    unsafe {
        let snpsid = read(GSNPSID);
        let revision = if matches!(snpsid >> 16, DWC31_IP | DWC32_IP) {
            read(VER_NUMBER) | DWC3_REVISION_IS_DWC31
        } else {
            snpsid
        };
        if revision < DWC3_REVISION_230A {
            return;
        }
        let devten = read(DEVTEN) | DEVTEN_SUSPEND;
        write(DEVTEN, devten);
        let _ = read(DEVTEN);
    }
}

/// Return the endpoint range used by qpr1's gadget start configuration.
///
/// qpr1's core probe first derives `dwc->num_eps` from GHWPARAMS3, but
/// `dwc3_gadget_init()` then sets it to `DWC3_ENDPOINTS_NUM` and allocates all
/// 32 endpoint objects. `dwc3_gadget_start_config()` consequently walks all 32
/// slots and skips only null objects. The direct handoff has no Linux
/// `dwc->eps[]` array, so mirror the actual qpr1 gadget-start range rather than
/// the earlier core-probe value.
#[inline]
fn qpr1_endpoint_count() -> usize {
    32
}

unsafe extern "C" {
    static __usb_dma_start: u8;
    static __usb_dma_end: u8;
    static __usb_trace_start: u8;
    static __usb_trace_end: u8;
}

const EVENT_BUFFER_SIZE: usize = 4096;
const MAX_PACKET_SIZE: u32 = 512;
// ADB advertises a 4 KiB transport window. The controller still uses a
// 512-byte USB packet size; this is the aggregate OUT request buffer that
// receives one complete ADB message before the protocol parser runs.
const DATA_OUT_BUFFER_SIZE: usize = 4096 + 24;
// Linux starts the gadget with the SuperSpeed EP0 descriptor size while the
// link speed is still unknown, then changes it to 64 on a High-Speed
// Connect Done event. The first SETUP transfer must use that initial state.
const INITIAL_EP0_MAX_PACKET_SIZE: u32 = 512;

// On Bramble the Android msm-eud driver disables the shared mode-manager
// resource through qcom_scm_io_writel(), then clears EUD CSR.EUD_EN directly.
// This is an opt-in handoff A/B only: the normal path keeps EUD ownership
// read-only and lets the HS-PHY source-exact gate decide whether to initialize.
const BRAMBLE_EUD_BASE: usize = 0x088e_0000;
const BRAMBLE_EUD_CSR_EUD_EN: usize = 0x1014;
const BRAMBLE_EUD_MODE_MANAGER: usize = 0x088e_2000;

#[cfg(fullerene_aarch64_usb_disable_eud)]
unsafe fn disable_eud_for_usb_handoff() -> u64 {
    let scm_result = if option_env!("FULLERENE_USB_EUD_SCM_SKIP") == Some("1") {
        trace_event(
            TRACE_PROBE_WATCHDOG,
            0x4544_534b, // "EDSK"
            BRAMBLE_EUD_MODE_MANAGER as u32,
            0,
            0,
            0,
        );
        log_puts("USB EUD SCM disable skipped (direct CSR-only)\n");
        0
    } else {
        watchdog::secure_scm_io_write(BRAMBLE_EUD_MODE_MANAGER, 0)
    };
    if scm_result != 0 {
        trace_event(
            TRACE_PROBE_WATCHDOG,
            0x4544_4641, // "EDFA"
            scm_result as u32,
            0,
            0,
            0,
        );
        return scm_result;
    }

    write_volatile((BRAMBLE_EUD_BASE + BRAMBLE_EUD_CSR_EUD_EN) as *mut u32, 0);
    core::arch::asm!("dsb sy", options(nostack));
    let csr = read_volatile((BRAMBLE_EUD_BASE + BRAMBLE_EUD_CSR_EUD_EN) as *const u32);
    trace_event(
        TRACE_PROBE_WATCHDOG,
        0x4544_4F4B, // "EDOK"
        csr,
        BRAMBLE_EUD_BASE as u32,
        BRAMBLE_EUD_CSR_EUD_EN as u32,
        0,
    );
    csr as u64
}

// The firmware-owned Fastboot event page is used only by the explicit
// --reuse-fastboot-dma differential. Keep every EP0 object inside that page
// so this test does not assume a second firmware allocation is accessible
// through the still-active SMMU context.
const FASTBOOT_EP0_EVENT_SIZE: usize = 0x100;
// Stock Bramble XBL's DwcCoreInit programs GEVNTSIZ0 with 0xf0 for the
// control event ring. Keep the backing allocation larger, but expose the
// exact hardware ring length in the gadget probe.
const XBL_EP0_EVENT_SIZE: usize = 0xf0;
// Stock Bramble XBL's DwcCoreInit publishes its event ring at this physical
// address. The A/B below uses it for the event ring only; setup/TRB/response
// objects stay in the known linker DMA pool.
const XBL_EP0_EVENT_DMA_ADDRESS: usize = 0x0a6f_c010;
// Stock XBL's initial EP0 request builder publishes this fixed DDR address
// for the first EP0 TRB. Its CONTROL_SETUP buffer field points here as well.
const XBL_EP0_TRB_DMA_ADDRESS: usize = 0x8079_8f70;
// Stock Bramble XBL's initial EP0 request builder reaches TRBCTL=2
// (CONTROL_SETUP). Keep the XBL-specific control word named here; the
// hardware A/B uses an explicit setup buffer because the literal zero pointer
// suppressed even the USB2 attach on this Fullerene handoff.
const TRB_XBL_EP0_SETUP: u32 = TRB_CONTROL_SETUP;
// Factory ABL's request builder starts every submitted TRB with HWO|CHN|ISP_IMI
// (0x405), leaving LST/IOC clear. This is intentionally an explicit A/B: it
// is not equivalent to adding CHN to the Linux LST|IOC form.
const TRB_ABL_REQUEST_FLAGS: u32 = TRB_HWO | TRB_CHN | TRB_ISP_IMI;
const FASTBOOT_EP0_TRB_OFFSET: usize = 0x140;
const FASTBOOT_EP0_RESPONSE_OFFSET: usize = 0x180;
const TRACE_FASTBOOT_EVENT_DMA: u32 = 39;

#[repr(C, align(4096))]
struct EventBuffer([u8; EVENT_BUFFER_SIZE]);

#[repr(C, align(64))]
struct ResponseBuffer([u8; 512]);

#[unsafe(link_section = ".usb_dma")]
static mut EVENTS: EventBuffer = EventBuffer([0; EVENT_BUFFER_SIZE]);
// Linux copies the producer-owned event ring into a CPU-owned cache before
// acknowledging GEVNTCOUNT.  Keep the same ownership boundary in the
// polling path; otherwise process_event() can issue a new endpoint command
// while it is still reading a ring slot that DWC3 may reuse after an ACK.
#[repr(C, align(4096))]
struct EventCache([u8; EVENT_BUFFER_SIZE]);

static mut EVENT_CACHE: EventCache = EventCache([0; EVENT_BUFFER_SIZE]);

#[unsafe(link_section = ".usb_dma")]
static mut GSI_EVENTS: [EventBuffer; 3] = [
    EventBuffer([0; EVENT_BUFFER_SIZE]),
    EventBuffer([0; EVENT_BUFFER_SIZE]),
    EventBuffer([0; EVENT_BUFFER_SIZE]),
];
#[unsafe(link_section = ".usb_dma")]
static mut EP0_TRBS: [Trb; 2] = [
    Trb {
        bpl: 0,
        bph: 0,
        size: 0,
        ctrl: 0,
    },
    Trb {
        bpl: 0,
        bph: 0,
        size: 0,
        ctrl: 0,
    },
];
/// qpr1's DWC3 CONTROL_SETUP path uses one DMA object for both roles: the
/// STARTTRANSFER command points at the EP0 TRB, and that TRB's buffer pointer
/// points back to the same eight-byte storage. Keep a separate buffer only as
/// an explicit non-source diagnostic differential.
#[repr(C, align(64))]
struct SetupBuffer([u8; 8]);

#[unsafe(link_section = ".usb_dma")]
static mut EP0_SETUP_BUFFER: SetupBuffer = SetupBuffer([0; 8]);
#[unsafe(link_section = ".usb_dma")]
static mut DATA_TRBS: [Trb; 2] = [
    Trb {
        bpl: 0,
        bph: 0,
        size: 0,
        ctrl: 0,
    },
    Trb {
        bpl: 0,
        bph: 0,
        size: 0,
        ctrl: 0,
    },
];
#[repr(C, align(64))]
struct DataBuffer([u8; DATA_OUT_BUFFER_SIZE]);

#[unsafe(link_section = ".usb_dma")]
static mut DATA_OUT_BUFFER: DataBuffer = DataBuffer([0; DATA_OUT_BUFFER_SIZE]);
#[unsafe(link_section = ".usb_dma")]
static mut RESPONSE: ResponseBuffer = ResponseBuffer([0; 512]);
static mut FASTBOOT_EVENT_DMA_BASE: u64 = 0;
static mut EVENT_OFFSET: usize = 0;
static mut GSI_EVENT_OFFSETS: [usize; 3] = [0; 3];
/// One retained request slot per Qualcomm event buffer. The Android GSI
/// wrapper is not a normal DWC3 ring: reusing a slot before its event arrives
/// would overwrite the TRB address that the wrapper is still consuming.
static mut GSI_PENDING: [bool; 3] = [false; 3];
static mut GSI_CHANNEL_ENDPOINT: [usize; 3] = [0; 3];
static mut GSI_CHANNEL_READY: [bool; 3] = [false; 3];
static mut GSI_REQUEST_SLOTS: [usize; 3] = [usize::MAX; 3];
static mut GSI_RING_BASES: [u64; 3] = [0; 3];
static mut GSI_RING_TRB_COUNTS: [usize; 3] = [0; 3];
static mut GSI_BUFFER_BASES: [u64; 3] = [0; 3];
static mut GSI_BUFFER_LENGTHS: [usize; 3] = [0; 3];
static mut GSI_DOORBELL_BASES: [u64; 3] = [0; 3];
static mut GSI_RESOURCE_INDEX: [u8; 3] = [0; 3];
static mut GSI_RING_ACTIVE: [bool; 3] = [false; 3];
static mut DMA_ALLOCATOR: Option<super::platform::bramble::DmaPoolAllocator> = None;

/// Latched signal-probe observables. The early Bramble handoff has no UART
/// and cannot enumerate, so these states are published to the host by
/// dropping the physical pull-up at a diagnostic delay (see
/// `ep0_signal_code()`); the host dmesg timestamps become the readout.
static mut SIGNAL_EVENT_DELIVERED: bool = false;
static mut SIGNAL_SETUP_TRB_RETIRED: bool = false;
static mut SIGNAL_SETUP_PACKET_RECEIVED: bool = false;
/// A host USB Reset is the first unambiguous on-wire boundary after the
/// inherited Fastboot session.  DSTS SOF/link fields can continue to change
/// while that old session is being torn down, so the signal readout must not
/// treat pre-reset samples as host traffic.
static mut SIGNAL_USB_RESET_SEEN: bool = false;
/// True once the DWC3 device-event stream delivered one of its own error
/// notifications. Keep this separate from TRACE_USB_DEVICE_ERROR, which is
/// also used for software/endpoint-command diagnostics and hibernation.
static mut SIGNAL_DWC3_DEVICE_ERROR: bool = false;
static mut SIGNAL_LAST_SOFFN: u16 = 0;
static mut SIGNAL_SOF_BASELINED: bool = false;
static mut SIGNAL_SOF_SEEN: bool = false;
/// Link-state ladder latches (see `ep0_link_signal_code()`).
static mut SIGNAL_LNKST_U0: bool = false;
static mut SIGNAL_LNKST_RESET: bool = false;
static mut SIGNAL_LNKST_POLLING: bool = false;
static mut SIGNAL_LNKST_RXDET: bool = false;
static mut SIGNAL_CORE_HALTED: bool = false;
/// True while the core owns an armed EP0 SETUP transfer. The core REJECTS
/// Start Transfer while the device link is not ON (including during the
/// host's bus reset), so the first arm attempt after Run/Stop completes with
/// "No resource" and must be retried once the link comes up; the poll-loop
/// guard uses this latch to re-arm exactly then, which also delivers any
/// SETUP packet the core latched while no TRB was armed.
static mut EP0_SETUP_ARMED: bool = false;
/// Host-visible result of the most recent direct EP0 SETUP arm attempt:
/// 0 = success, 1 = skipped by the DSTS HALT gate, 2 = completed with a
/// non-zero STARTTRANSFER status, 3 = STARTTRANSFER timed out, 4 = no
/// classified attempt. The
/// arm-blip diagnostic encodes these as one, two, three, or four Run/Stop
/// pairs respectively, so a host-only capture can identify the pre-EP0
/// failure stage without a UART or configfs operation.
static mut SETUP_ARM_FAILURE_STAGE: u32 = 4;
/// Raw completion word for the most recent EP0 STARTTRANSFER. The high bit is
/// reserved as the timeout marker; 0xffff_ffff means no command has retired.
static mut SETUP_ARM_LAST_COMMAND: u32 = 0xffff_ffff;
/// Defer the post-Run/Stop event-DMA probe until the live host link reaches
/// U0. Run/Stop returns before Bramble's host-side attach debounce completes,
/// so an init-time U0 check can finish too early and test a disconnected
/// controller rather than the event path used by enumeration.
static mut POST_RUNSTOP_PROBE_PENDING: bool = false;
static mut POST_RUNSTOP_PROBE_NOT_BEFORE: u64 = 0;
const POST_RUNSTOP_PROBE_DELAY_SECS: u64 = 8;

/// Selector for the deferred attach-time one-bit readout.
///
/// The immediate `--utmi-postrun-readout` dispatch runs *inside* the handoff,
/// i.e. before the host attaches and before the handoff's own `DEVTEN` publish
/// (both live in `init_usb2_gadget_reuse_fastboot_ep0()`, the publish ~180 lines
/// after the dispatch). Readings taken there therefore cannot answer anything
/// about the state the host actually sees - run `307410.0` ("the event ring is
/// untouched") and runs `326253.0`/`329043.0` ("`DEVTEN == 0`") were all taken at
/// that early point, which is why the first was retracted and the second proved
/// not to matter (a plain run without any readout fails identically,
/// `334730.0`). This deferred point runs in the polling owner
/// `POST_RUNSTOP_PROBE_DELAY_SECS` after the handoff - after the attach and after
/// the bus reset - and publishes one bit over the same CCS channel.
///
/// 0 = none, 1 = event ring memory non-zero, 2 = `DEVTEN != 0`.
static mut DEFERRED_READOUT_KIND: u32 = 0;
/// One-shot guard for the `"DEFR"` retained-trace marker in the deferred block.
static mut DEFR_MARKED: bool = false;

/// Last `DSTS.SOFFN` seen by the polling owner, used as the attach detector for
/// the deferred readout: the frame number only advances once the host is really
/// driving the bus (see the deferred block in `poll()`).
static mut LAST_DEFERRED_SOFFN: u32 = 0xffff_ffff;

/// True once the deferred trigger has been armed with the frame number observed
/// at the first poll after the handoff; only then does a change fire the action.
static mut DEFERRED_SOFFN_ARMED: bool = false;
/// Set by the USB Reset / Connect Done handlers: the host is present and the
/// link is coming up, so the guard should arm the SETUP TRB (retrying with a
/// small cooldown until the link reaches ON). Arming is deliberately NOT
/// attempted before the first USB Reset: the core rejects Start Transfer
/// while disconnected, and millions of failed commands during the pre-attach
/// window can wedge the endpoint command engine.
static mut PENDING_SETUP_ARM: bool = false;
/// Poll retries to skip after a failed SETUP arm. The core fast-fails Start
/// Transfer with "No resource" while the link is not ON; hammering the
/// command engine at poll rate during that window can wedge it, so the
/// guard backs off between attempts.
static mut ARM_COOLDOWN: u32 = 0;
/// Recovery-only EP0 data-phase arm. Android msm 4.19 starts the data
/// transfer immediately when the gadget queues the response. This flag is
/// set only if that initial STARTTRANSFER fails; a later
/// XferNotReady(CONTROL_DATA) event then provides a bounded retry point.
/// Which `ControlAction` `GadgetDriver::on_setup` returned, latched by `handle_setup()`.
/// 0 = never written. See usb/README.md §3.7.
static mut LAST_CONTROL_ACTION: u8 = 0;
static mut DATA_PHASE_PENDING_START: bool = false;
static mut DATA_PHASE_PENDING_LEN: usize = 0;
/// CNTPCT tick of the first successful post-connect Run/Stop (quiet-window
/// reference; 0 = no start recorded yet).
static mut RUN_STOP_TICK: u64 = 0;
/// Bounded late recovery for the Bramble USB2 arm-window A/B. The host can
/// take tens of seconds to reach the HS attach after Run/Stop, so an init-time
/// arm result alone is not enough to decide whether the controller needs a
/// soft reset before the first descriptor request. A failed recovery is
/// retried only while EP0 is still unarmed; a successful arm/configuration
/// stops the sequence immediately.
static mut ARM_WINDOW_RECOVERY_ATTEMPTS: u8 = 0;
const ARM_WINDOW_RECOVERY_DELAY_SECS: u64 = 30;
const ARM_WINDOW_RECOVERY_RETRY_SECS: u64 = 4;
const ARM_WINDOW_RECOVERY_MAX_ATTEMPTS: u8 = 6;
/// Connect-delay one-shot latch (see the delay block in
/// `init_with_super_speed`). Only the first handoff attempt pays the delay
/// so the retry loop stays inside the EL1 recovery-timer budget.
static mut SIGNAL_CONNECT_DELAYED: bool = false;
/// Adopted SMMU mapping (see `adopt_smmu_dma_mapping()`). When the Apps-SMMU
/// stream is owned by a live TRANSLATE context that software cannot rewrite,
/// the EP0 DMA objects are relocated into a page that context already maps:
/// the CPU addresses the page at `DMA_ADOPTED_CPU` while DWC3 is published
/// the corresponding IOVA in `DMA_ADOPTED_IOVA`.
// CRATE-SHARED, not plain `static mut`: an address-selecting static must not exist twice.
// `usb_probe.rs` compiles `usb/` a second time, and `DMA_ADOPTED` decides whether
// `ep0_setup_data_ptr()` returns the linker's `EP0_SETUP_BUFFER` or `ep0_trb_ptr(0)` inside
// the adopted SMMU page. Two copies meant the handoff set it in the probe crate while the
// kernel crate's copy stayed false, so the two crates read different physical memory.
// See usb/README.md §1.2.
use self::trace::{
    SHARED_DMA_ADOPTED as DMA_ADOPTED, SHARED_DMA_ADOPTED_CPU as DMA_ADOPTED_CPU,
    SHARED_DMA_ADOPTED_IOVA as DMA_ADOPTED_IOVA,
};

#[inline]
fn dma_mapping_adopted() -> bool {
    unsafe { DMA_ADOPTED }
}

/// Translate a CPU-side pointer inside the adopted page into the IOVA that
/// DWC3 must use. Outside adopted mode the CPU address IS the DMA address.
#[inline]
unsafe fn dma_iova_for(cpu: usize) -> u64 {
    unsafe {
        if DMA_ADOPTED {
            DMA_ADOPTED_IOVA + (cpu - DMA_ADOPTED_CPU) as u64
        } else {
            cpu as u64
        }
    }
}

/// Newest STARTTRANSFER outcome harvested from the retained trace of the
/// previous attempts (0xFFFF_FFFF = none; bit 31 set = the command timed out;
/// otherwise the raw DEPCMD register: status in bits 15:12).
pub fn harvest_last_str_code() -> u32 {
    unsafe { TRACE_HARVEST_LAST }
}

/// Return a host-visible encoding of the post-Run/Stop event-DMA probe.
/// `0xffff_ffff` means the probe record was never emitted; otherwise bits
/// 0..2 are GETEPSTATE command success, EP0 still armed, and event-ring
/// delivery. The caller maps no-record to 9 and a recorded bitmask to
/// bitmask+1 so that the distinction survives the pull-up attach-count
/// channel.
pub fn post_event_dma_readout_code() -> u32 {
    unsafe {
        harvest_trace_outcome();
        if TRACE_HARVEST_POST == 0xffff_ffff {
            9
        } else {
            (TRACE_HARVEST_POST & 0x7).saturating_add(1)
        }
    }
}

#[inline]
unsafe fn ep0_event_dma_base() -> usize {
    unsafe {
        if cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_event_dma) {
            return XBL_EP0_EVENT_DMA_ADDRESS;
        }
        if DMA_ADOPTED {
            return DMA_ADOPTED_CPU;
        }
        let captured = FASTBOOT_EVENT_DMA_BASE;
        if cfg!(fullerene_aarch64_usb_gadget_handoff_reuse_fastboot_dma) && captured != 0 {
            captured as usize
        } else {
            addr_of_mut!(EVENTS) as usize
        }
    }
}

#[inline]
unsafe fn ep0_event_address() -> u64 {
    unsafe {
        if DMA_ADOPTED {
            return DMA_ADOPTED_IOVA;
        }
        ep0_event_dma_base() as u64
    }
}

#[inline]
unsafe fn ep0_event_size() -> usize {
    unsafe {
        if cfg!(fullerene_aarch64_usb_gadget_handoff_event_ring_size_4096) {
            return EVENT_BUFFER_SIZE;
        } else if DMA_ADOPTED {
            return XBL_EP0_EVENT_SIZE;
        }
        if cfg!(fullerene_aarch64_usb_gadget_handoff_reuse_fastboot_dma)
            && FASTBOOT_EVENT_DMA_BASE != 0
        {
            XBL_EP0_EVENT_SIZE
        } else if cfg!(fullerene_aarch64_usb_gadget_handoff_probe) {
            // Android msm/qpr1 allocates every DWC3 event buffer with
            // DWC3_EVENT_BUFFERS_SIZE (4096). The historical 0xf0 value is
            // an ABL observation and is valid only when reusing that
            // firmware-owned event buffer; it is not the source default for
            // the linker-owned direct handoff ring.
            EVENT_BUFFER_SIZE
        } else {
            EVENT_BUFFER_SIZE
        }
    }
}

#[inline]
unsafe fn ep0_trb_ptr(index: usize) -> *mut Trb {
    unsafe {
        if cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_stock_ep0_dma) {
            return (XBL_EP0_TRB_DMA_ADDRESS + index * core::mem::size_of::<Trb>()) as *mut Trb;
        }
        if DMA_ADOPTED {
            return (DMA_ADOPTED_CPU as *mut u8)
                .add(FASTBOOT_EP0_TRB_OFFSET + index * core::mem::size_of::<Trb>())
                .cast::<Trb>();
        }
        if cfg!(fullerene_aarch64_usb_gadget_handoff_reuse_fastboot_dma)
            && FASTBOOT_EVENT_DMA_BASE != 0
        {
            (ep0_event_dma_base() as *mut u8)
                .add(FASTBOOT_EP0_TRB_OFFSET + index * core::mem::size_of::<Trb>())
                .cast::<Trb>()
        } else {
            addr_of_mut!(EP0_TRBS).cast::<Trb>().add(index)
        }
    }
}

/// Return the DMA target for the EP0 CONTROL_SETUP transfer.
///
/// qpr1's `dwc3_ep0_out_start()` aliases the eight-byte setup request to
/// `ep0_trb_addr`; the STARTTRANSFER command and CONTROL_SETUP TRB therefore
/// use the same DMA object. The separate-buffer branch is retained only as an
/// explicit non-source diagnostic, while adopted/reused Fastboot pages keep
/// their firmware-owned layout.
/// Short host-visible pulse: 50 ms stop + 50 ms run.
///
/// A full `ccs_pulse(300)` costs ~500 ms (stop, 300 ms, run, 200 ms) and the EP0 arm window is
/// 400 ms, so a breadcrumb placed inside that window changes the timing of the code it measures.
/// That is why the level-9 and level-10 readings disagreed. 100 ms is still host-visible - the
/// `usb2-live-halted` selector uses a 150 ms variant - and perturbs five times less.
/// See usb/README.md §3.29.
macro_rules! bcp_short {
    ($slot:expr) => {
        if matches!(
            option_env!("FULLERENE_USB_PULSE_BREADCRUMB"),
            Some("11") | Some("4")
        ) {
            let bcp_slot = $slot as usize;
            if bcp_slot < 24 && unsafe { !trace::BC_ONCE[bcp_slot] } {
                unsafe {
                    trace::BC_ONCE[bcp_slot] = true;
                    let _ = run_stop_device(false);
                    readout_keepalive_delay_ms(50);
                    let _ = run_stop_device(true);
                    readout_keepalive_delay_ms(50);
                }
            }
        }
    };
}

/// Pull-up marker, delayed: wait `delay_ms`, then emit `edges` as `pullup_mark` would.
///
/// Why this exists: `pullup_mark` holds each state 150 ms, so `pullup_mark(2)` is 600 ms of
/// attempts and `pullup_mark(8)` is 2.4 s. That makes a longer train test two things at once - elapsed
/// time AND eight connect/disconnect cycles, which the host may not report at all. Measured
/// 2026-09-29 ~11:25: eight edges at an unconditional site gave rows=2/pulses=0 (3/3) while two
/// edges microseconds away gave rows=4/pulses=1 (4/4), so the train's length, not its position,
/// changed the answer. This variant separates them - the edge count stays the same and only the
/// start time moves.
pub fn pullup_mark_after(delay_ms: u64, edges: u32) {
    #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
    {
        if delay_ms != 0 {
            super::timer::delay_ms(delay_ms);
        }
        pullup_mark(edges);
    }
    #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
    {
        let _ = (delay_ms, edges);
    }
}

/// Screen-marker helper for the probe: alternate the panel between white and black.
///
/// The probe has no UART - `usb/log.rs` compiles its logging out entirely under
/// `fullerene_aarch64_usb_gadget_handoff_probe` - and the USB pull-up does not rise until the
/// handoff reaches its Run/Stop, so between `fastboot boot` and the attach there is no channel the
/// host can see. The panel is the exception: `display::dpu` can paint the active scanout buffer in
/// place, so a colour change is an observable timestamp.
///
/// Call this at each checkpoint. With a webcam recording the handset, the interval between two
/// flips is the time spent between them, which is the measurement that has been missing: measured
/// 2026-09-29, `fastboot boot` -> attach is 40 s against a ~25 s budget, so ~15 s has to be found
/// in the probe's own startup. See usb/README.md §3.31.
/// Pull-up marker: raise and drop the D+/D- pull-up `edges` times so the host records each one.
///
/// The panel turned out not to be a channel: `dpupaint` paints the active scanout buffer and parks
/// 90 s (`usb_probe.rs:1006-1022`), yet 120 frames of 2 fps capture at 1920x1080 showed the screen
/// never left ambient brightness (YAVG 55-81, zero frames above 180). `active_layer()` returns
/// `None` under the probe, which is what `usb_probe.rs:1980` already warned about.
///
/// The pull-up is the one output already known to reach the host: a `USB2EXT` run produced exactly
/// `09:06:06.314 new high-speed USB device` and nothing else, one edge. `run_stop_device` is defined
/// at `control.rs:420` and mod.rs already toggles it in trains (`:8228-8234` etc.) - but none of
/// those trains has ever been observed at the host. This helper makes a train observable on demand,
/// at a place of the caller's choosing, so the question "is `:8228` reached?" becomes "did an edge
/// appear, and when?".
///
/// Each state is held for `USB_PULLUP_MARK_HOLD_MS`. That is deliberately far longer than the host
/// needs to notice an electrical change, because the host is running a five-second enumeration
/// timeout per attach and a train that is too fast would collapse into a single edge and be
/// indistinguishable from no train at all.
///
/// Callers should treat the edge count, not the return value, as the measurement: `run_stop_device`
/// returning true says the register write was issued, not that the host saw anything.
pub fn pullup_mark(edges: u32) {
    #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
    {
        const USB_PULLUP_MARK_HOLD_MS: u64 = 150;
        let mut n = 0;
        while n < edges {
            let _ = unsafe { control::run_stop_device(false) };
            super::timer::delay_ms(USB_PULLUP_MARK_HOLD_MS);
            let _ = unsafe { control::run_stop_device(true) };
            super::timer::delay_ms(USB_PULLUP_MARK_HOLD_MS);
            n += 1;
        }
    }
    #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
    {
        let _ = edges;
    }
}

pub fn screen_mark(slot: u32) {
    #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
    unsafe {
        // Even slots white, odd slots black: the sequence of transitions is monotone, so a missed
        // frame in the recording cannot produce a false count.
        let fill = if slot % 2 == 0 {
            0xffff_ffff
        } else {
            0x0000_0000
        };
        let _ = crate::display::dpu::paint_active_framebuffer(fill);
    }
    #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
    {
        let _ = slot;
    }
}

#[inline]
unsafe fn ep0_setup_data_ptr() -> *mut u8 {
    unsafe {
        // INSIDE THE POINTER HELPER (level 11). Short pulses: a full `ccs_pulse(300)` costs ~500 ms
        // against a 400 ms arm window, so observing the handoff changed the handoff - which is what
        // made level 9 and level 10 disagree. A 50 ms stop + 50 ms run is still host-visible (the
        // 150 ms variant is used by `usb2-live-halted`) and perturbs five times less.
        // See usb/README.md §3.29.
        bcp_short!(20);
        // `usb2-live-setup-inpage`: the handoff adopts exactly ONE SMMU page and
        // that page is the one holding the firmware's EP0 TRB ring
        // (`usb2-live-dma-window`, run `291878.0`: pulse 1 present, pulse 2
        // absent, pulse 3 absent). `dma_iova_for()` is a linear offset from that
        // page, so it is only valid inside it, and the linker-allocated
        // `EP0_SETUP_BUFFER` is outside - which is why the controller could
        // report `SETUP_PENDING` (`278916.0`) with the software's buffer empty
        // (`280408.0`). Use the second TRB slot: inside the adopted page, and
        // the handoff already cleans two TRB slots (`usb.rs:6553-6557`).
        if option_env!("FULLERENE_USB_UTMI_POSTRUN_READOUT") == Some("usb2-live-setup-inpage") {
            return ep0_trb_ptr(1).cast::<u8>();
        }
        bcp_short!(21);
        // The `if`/`else` is lifted into a binding so the short pulse can sit between the buffer
        // selection and the return without turning the enclosing block's value into `()`.
        let buffer = if cfg!(fullerene_aarch64_usb_gadget_handoff_ss_separate_setup_buffer)
            && !cfg!(fullerene_aarch64_usb_abl_setup_trb_buffer)
            && !DMA_ADOPTED
            && !(cfg!(fullerene_aarch64_usb_gadget_handoff_reuse_fastboot_dma)
                && FASTBOOT_EVENT_DMA_BASE != 0)
        {
            addr_of_mut!(EP0_SETUP_BUFFER.0).cast::<u8>()
        } else {
            ep0_trb_ptr(0).cast::<u8>()
        };
        bcp_short!(22);
        // `bcp_short!(23)` belongs *inside* the `unsafe` block: leaving it after the closing
        // brace made the function's tail expression `()` instead of the pointer.
        bcp_short!(23);
        buffer
    }
}

#[inline]
fn ep0_trb_index(endpoint: usize) -> usize {
    if cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_direction_trb) && endpoint == 1 {
        1
    } else {
        0
    }
}

#[inline]
unsafe fn ep0_response_ptr() -> *mut u8 {
    unsafe {
        if DMA_ADOPTED {
            return (DMA_ADOPTED_CPU as *mut u8).add(FASTBOOT_EP0_RESPONSE_OFFSET);
        }
        if cfg!(fullerene_aarch64_usb_gadget_handoff_reuse_fastboot_dma)
            && FASTBOOT_EVENT_DMA_BASE != 0
        {
            (ep0_event_dma_base() as *mut u8).add(FASTBOOT_EP0_RESPONSE_OFFSET)
        } else {
            addr_of_mut!(RESPONSE.0).cast::<u8>()
        }
    }
}

static mut EP0_STATE: Ep0State = Ep0State::Setup;
static mut CONTROL_IN: bool = false;
static mut CONTROL_HAS_DATA: bool = false;
static mut CONFIGURED: bool = false;
// The standalone handoff probe has a recovery deadline for the no-host case,
// but an idle, successfully-serviced EP0 is a valid steady state. Keep this
// separate from CONFIGURED: a descriptor-only host may never issue
// SET_CONFIGURATION while EP0 is nevertheless healthy.
static mut PROBE_EP0_PROGRESS: bool = false;
static mut ENDPOINTS_READY: bool = false;
/// Set at the instant the endpoint-config block publishes `ENDPOINTS_READY = true`.
/// Read only by the `usb2-live-ep0-armed-order` readout, which compares it with the
/// live value to tell "the block never ran" apart from "it ran and something cleared
/// the flag". A pulse cannot be used for the first half: that code runs while the
/// controller is still halted, and the pulse channel needs a live port.
static mut ENDPOINT_CONFIG_BLOCK_REACHED: bool = false;
/// Bit 1 of the same two-bit readout: set just before the gadget-start branch, so a
/// single run separates "this epoch was never reached" from "it was reached and the
/// false branch was taken". Recorded, not pulsed, for the same reason as above.
static mut ENDPOINT_CONFIG_EPOCH_REACHED: bool = false;
/// Monotone milestone index for the endpoint-publication region of
/// `init_usb2_gadget_reuse_fastboot_ep0`. Published one bit at a time as
/// ">= k" through the `usb2-live-milestone-ge-*` selectors, never as a count.
///
/// The region between `ENDPOINTS_READY = false` and `ENDPOINTS_READY = true`
/// contains eight early exits. The recorded failure stage is 0, which rules out
/// every `gadget_handoff_fail(n)`, so the exit is one of the stage-probe checks.
///   1 = just after ENDPOINTS_READY = false
///   2 = past stage-4 probe
///   3 = past stage-9 probe
///   4 = past stage-10 probe
///   5 = past stage-8 probe
///   6 = past the EP0-IN configure
///   7 = reached ENDPOINTS_READY = true
static mut HANDOFF_MILESTONE: u8 = 0;
/// How many event words `process_event` has consumed this boot, saturating.
/// Published one bit at a time ("at least N") by `usb2-live-events-ge-*`.
static mut EVENTS_CONSUMED: u16 = 0;
/// Set if any consumed event decoded as a device event of kind 0 (Disconnect),
/// i.e. the branch that clears `ENDPOINTS_READY` (`mod.rs:4813`).
static mut DISCONNECT_SEEN: bool = false;
/// The raw word of the first event that decoded as Disconnect, for decoding
/// against the vendor's `core.h` layout.
static mut DISCONNECT_RAW: u32 = 0;
/// Set if a Type-C `DetachDetected` event was applied. That branch
/// (`mod.rs:5446`) is the only remaining unconditional clear of `ENDPOINTS_READY`
/// reachable on this profile once `process_event` is ruled out (no events are
/// consumed at all).
static mut TYPEC_DETACH_SEEN: bool = false;
/// Monotone progress index for the USB2 handoff entry point, published only by
/// `usb2-live-handoff-progress`. It exists to answer one question in a single run:
/// *where* does `init_usb2_gadget_reuse_fastboot_ep0` leave early?
///
/// This is deliberately a progress *index*, not a set of predicates: the value is
/// written by straight-line code at each milestone, never rewound, and read exactly
/// once. So "N pulses = milestone N was the last one reached" has one reading and no
/// ordering ambiguity - unlike the assign-and-retract predicate encodings this skill
/// retired. Do not reuse the pattern for anything non-monotone.
///
/// Milestones: 0 entry, 1 smmu bypass, 2 stage-1 probe, 3 bare pull-up,
/// 4 controller block reset, 5 core reset, 6 SMMU ready, 7 stage-3 probe,
/// 8 configure_gadget_start_defaults, 9 ENDPOINTS_READY = true.
static mut HANDOFF_PROGRESS: u8 = 0;
/// One bit: was `init_usb2_gadget_reuse_fastboot_ep0` entered at all? Published by
/// `usb2-live-handoff-progress`. This is the only question that readout answers now -
/// the multi-pulse progress version was retracted once `usb2-live-pulse-calibration`
/// showed four pulses produce a single attach line (see the retraction entry in the
/// skill). Everything finer has to be bisected, one bit and one run at a time.
static mut HANDOFF_ENTERED: bool = false;
/// Set at the one place the direct Fastboot-reuse path asserts
/// `GUSB2PHYCFG0.SUSPHY` (mod.rs:7139). `HANDOFF_MILESTONE >= 1` proves execution
/// reaches 7359, which is *after* 7139, so this bit should be TRUE - if it is
/// FALSE the milestone reading and the register reading disagree.
static mut SUSPHY_SET_IN_HANDOFF: bool = false;
/// Raw `GUSB2PHYCFG0` as read *before* the SUSPHY write at the handoff site.
static mut SUSPHY_RAW_BEFORE: u32 = 0;
/// Raw `GUSB2PHYCFG0` as read back *immediately after* that write. If this does
/// not have SUSPHY set, the register's readback path is the problem.
static mut SUSPHY_RAW_AFTER: u32 = 0;
/// Set when `send_ep_command_result` actually clears `SUSPHY`/`ENBLSLPM` for a
/// command, i.e. when `saved_usb2_config != 0`. If this is FALSE the guard never
/// engaged and there was never anything for the restore to put back.
static mut CMD_GUARD_ENGAGED: bool = false;

/// Record the last handoff milestone reached. Unconditional and cheap - only
/// `usb2-live-handoff-progress` ever publishes the value, so for every other build
/// this is one store with no observable effect.
#[inline]
fn handoff_progress(milestone: u8) {
    unsafe { HANDOFF_PROGRESS = milestone };
    // ALSO publish into the retained DRAM trace so it can be read crate-independently:
    // `HANDOFF_PROGRESS` is a `static mut` in `usb/`, and `usb_probe.rs` compiles `usb/`
    // again, so a reader there sees a different copy. "HOP" + milestone, so the decoder can
    // report the HIGHEST milestone reached rather than just the last one written.
    trace_marker(TRACE_PROBE_WATCHDOG, 0x484F_5000 | u32::from(milestone));
}
static mut DATA_ENDPOINTS_READY: bool = false;
static mut DATA_REQUEST_SLOTS: [usize; 2] = [usize::MAX; 2];
/// DWC3 returns a resource index for every STARTTRANSFER, including normal
/// bulk endpoints. Keep it per endpoint so ENDTRANSFER remains valid after
/// a second queue/rearm cycle instead of relying on the first index.
static mut DATA_RESOURCE_INDEX: [u8; 2] = [0; 2];
/// True when the currently bound gadget function owns a GSI channel instead
/// of the ordinary DWC3 bulk pair. Keep this separate from
/// `DATA_ENDPOINTS_READY`: both paths share the gadget bind lifetime, but
/// their completion and teardown rules differ.
static mut GSI_GADGET_BOUND: bool = false;
static mut FUNCTION_BOUND: bool = false;
/// DWC3 returns a transfer-resource index from STARTTRANSFER.  Linux retains
/// it per endpoint and supplies it to ENDTRANSFER; using a fixed value works
/// only accidentally on the first controller generation.
static mut EP0_RESOURCE_INDEX: [u8; 2] = [0; 2];
/// Software ownership slot for the historical XBL differential. This is not
/// the canonical initial-SETUP arm point: Android msm/Linux prepare the
/// CONTROL_SETUP TRB and issue STARTTRANSFER before the controller can emit
/// later phase notifications.
static mut EP0_SETUP_REQUEST_SLOT: usize = usize::MAX;
/// Failure stage for the standalone gadget handoff probe. The probe uses
/// this to make a retained failure host-observable without publishing a
/// broken USB pull-up.
#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
static mut GADGET_HANDOFF_FAILURE_STAGE: u32 = 0;
// Direct-path (init_with_super_speed) EP command diagnostic snapshot. The
// direct path uses plain `return false` (no GADGET_HANDOFF_FAILURE_STAGE), so
// capture how far it gets, the raw DEPCTL after each command (CMDACT bit 10
// still set == the core never retired the command), and the core-state DSTS
// at the first endpoint command. 0xFFFF_FFFF = not reached.
static mut INIT_STAGE: u32 = 0;
// Post-init-failure self-heal outcome (u0_arm_recovery): 0xFFFF_FFFF = not
// run, 0 = EP0 armed, 1 = run/stop failed, 4 = DEPSTARTCFG failed,
// 5 = EP0-OUT config failed, 6 = EP0-IN config failed, 8 = SETUP TRB arm
// failed (pre-Run/Stop; the poll loop retries it after the link is ON).
static mut U0_ARM_STATUS: u32 = 0xFFFF_FFFF;
// Host-visible blip count to emit once the link reaches ON (see
// try_u0_blip). Set by the signal probe from the u0_arm_recovery status;
// the poll loop clears it after the blips are emitted.
static mut U0_BLIP_PENDING: u32 = 0;
/// Fallback deadline for the arm-stage blip. The handoff can publish the
/// physical session well before the host's HS attach debounce, so a blip
/// emitted synchronously may be invisible to usbmon. After this deadline the
/// categorical marker is emitted even if the core never reports U0.
static mut ARM_BLIP_FORCE_DEADLINE: u64 = 0;
/// Set once `arm_blip_queue` passes its compile-time gate. Distinguishes "the
/// A/B was not compiled in" from "it was compiled in but never called".
static mut ARM_BLIP_QUEUED: bool = false;
/// Set once `runstop_blips` actually toggled Run/Stop for this A/B.
static mut ARM_BLIP_DONE: bool = false;
// Set by link_on_sample (called from poll) once the core's own link FSM
// reads U0 (USBLNKST == 0 on a running, unhalted core), for the
// "lnk-ever-on" gate: distinguishes a persistent link-FSM desync from a
// U0 that was reached and then dropped.
static mut LNK_EVER_ON: bool = false;
// Set by link_on_sample (called from poll) once the core's own link FSM
// reads a mid-transaction state (USBLNKST in {RECOV=8, HRESET=9, LPBK=11,
// RESET=14, RESUME=15}), for the "lnk3" gate: distinguishes a core whose
// UTMI RX never saw the host's reset (FSM never woke, latch stays false)
// from one stuck in the reset handshake (RX alive, latch true).
static mut LNK_MID_SEEN: bool = false;
static mut INIT_PRE_RESET_DSTS: u32 = 0xFFFF_FFFF;
static mut INIT_DEPSTART_PRE_DSTS: u32 = 0xFFFF_FFFF;
static mut INIT_DEPSTART_RAW: u32 = 0xFFFF_FFFF;
static mut INIT_DEPSTART_DSTS: u32 = 0xFFFF_FFFF;
static mut INIT_EPCFG0_OK: bool = false;
static mut INIT_EPCFG0_RAW: u32 = 0xFFFF_FFFF;
static mut INIT_EPCFG0_DSTS: u32 = 0xFFFF_FFFF;
static mut INIT_EPCFG1_OK: bool = false;
static mut INIT_EPCFG1_RAW: u32 = 0xFFFF_FFFF;
static mut INIT_EPCFG1_DSTS: u32 = 0xFFFF_FFFF;
static mut TYPEC_LANE_B: bool = false;
/// True only after the combo QMP PHY has completed its cold initialization.
/// USB2 handoff deliberately keeps this false: the USB2 path must not touch
/// SuperSpeed-only autonomous-mode registers owned by the bootloader.
static mut QMP_PHY_READY: bool = false;
// Snapshot captured at an explicitly selected SS boundary. Stage 20 captures
// the pre-production Run/Stop state; stage 21 captures the immediate
// post-Run/Stop state. The standalone probe cannot rely on UART or warm-reset
// DRAM after recovery, so the signal gate reads this same-boot copy instead of
// sampling a later state that the host may already have changed.
static mut SS_STATE_SNAPSHOT_VALID: bool = false;
static mut SS_STATE_SNAPSHOT_DSTS: u32 = 0xffff_ffff;
static mut SS_STATE_SNAPSHOT_DCTL: u32 = 0xffff_ffff;
static mut SS_STATE_SNAPSHOT_PIPE: u32 = 0xffff_ffff;
static mut SS_STATE_SNAPSHOT_GCTL: u32 = 0xffff_ffff;
static mut SS_STATE_SNAPSHOT_QSCRATCH: u32 = 0xffff_ffff;
static mut SS_STATE_SNAPSHOT_QSCRATCH_GENERAL: u32 = 0xffff_ffff;
static mut SS_STATE_SNAPSHOT_QMP: u32 = 0xffff_ffff;
static mut SS_STATE_SNAPSHOT_QMP_STATUS2: u32 = 0xffff_ffff;
static mut SS_STATE_SNAPSHOT_QMP_POWER: u32 = 0xffff_ffff;
static mut SS_STATE_SNAPSHOT_QMP_BRANCHES: [u32; 4] = [0xffff_ffff; 4];
static mut SS_STATE_SNAPSHOT_LTSSM: u32 = 0xffff_ffff;
// Controller-domain snapshot bits: bit 0 = DWC3 GSNPSID responds with a
// known revision, bit 1 = USB30 GDSC PWR_ON, bit 2 = GCC core branch is on,
// bit 3 = GCC mock-UTMI branch is on.  The source/config words are retained
// in the trace records emitted by capture_ss_state_snapshot().
static mut SS_STATE_SNAPSHOT_DOMAIN: u32 = 0;
// Same-boot Run/Stop differential: bit 0 is the DCTL.RUN_STOP state just
// before the production write, bit 1 is the immediate post-write state, bit
// 2 is the later stage-21 snapshot, and bit 3 is the stage-21 DSTS halt bit.
// 0xffff_ffff means the production transition was not instrumented.
static mut SS_RUNSTOP_PRE_DCTL: u32 = 0xffff_ffff;
static mut SS_RUNSTOP_POST_DCTL: u32 = 0xffff_ffff;
static mut SS_RUNSTOP_POST_DSTS: u32 = 0xffff_ffff;
static mut TYPEC_STATE_VALID: bool = false;
static mut TYPEC_STATE: super::platform::bramble::TypecState =
    super::platform::bramble::TypecState {
        arbiter_version: 0,
        apid: 0,
        writable: false,
        misc_status: 0,
        mode: 0,
        orientation_reverse: false,
        role: super::platform::bramble::UsbRole::None,
        sink_mode_written: false,
        attached: false,
        attach_settled: false,
        phase: super::platform::bramble::TypecPhase::Disabled,
    };
static mut TYPEC_POLL_TICKS: u32 = 0;
/// A Type-C parent SPI is a hard-IRQ notification; the SPMI child/arbiter
/// transaction belongs to the deferred role-switch context. Keep this bit
/// separate so a slow PMIC access cannot run inside DWC3 IRQ handling.
static mut TYPEC_IRQ_PENDING: bool = false;
/// A Qualcomm power-event IRQ is handled synchronously by the early exception
/// path, while Linux runs the corresponding handler in a threaded IRQ/work
/// context.  Defer the potentially long clock/PHY/controller resume until
/// poll() so an IRQ cannot execute a full runtime transition in exception
/// context.
static mut RESUME_PENDING: bool = false;
static mut USB_IN_P3: bool = false;
static mut USB_RUNTIME_STATE: super::platform::bramble::UsbRuntimeState =
    super::platform::bramble::UsbRuntimeState::Off;
/// Next USB2 runtime power-domain keepalive deadline.  The direct Bramble
/// handoff inherits Fastboot's active USB session, but Fastboot's RPMh votes
/// disappear when ABL exits; the Android glue reasserts its performance/power
/// contract from runtime work while the early image has no workqueue yet.
#[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_runtime_power_keepalive)]
static mut USB2_RUNTIME_KEEPALIVE_NEXT: u64 = 0;
#[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_runtime_power_keepalive)]
static mut USB2_RUNTIME_KEEPALIVE_COUNT: u32 = 0;
// When the normal Android-init path takes the USB handoff before MMU/UFS
// setup, the storage transaction must not starve the event-ring consumer.
// This flag is set only after a successful early handoff; ordinary and
// failed-handoff paths therefore keep their existing ownership rules.
static mut EARLY_HANDOFF_ACTIVE: bool = false;
// Keep a reset-safe diagnostic bit while the normal Rust entry path is
// executing the Bramble handoff. If a secure-owned MMIO access raises a
// synchronous exception in this window, the normal exception vector would
// otherwise park forever at WFE and leave the phone on the Google logo.
static mut EARLY_HANDOFF_IN_PROGRESS: bool = false;
/// The gadget driver is deliberately independent of DWC3 registers.  The
/// hardware UDC feeds it setup/complete callbacks, while the QEMU simulator
/// uses the same request/state implementation directly.
static mut GADGET: Ep0Simulator = Ep0Simulator::new();
static mut UDC: UsbUdc = UsbUdc::new();

#[cfg(fullerene_aarch64_bramble)]
const IMEM_RESTART_REASON: usize = 0x146a_b65c;
#[cfg(fullerene_aarch64_bramble)]
const IMEM_BOOTLOADER_REASON: u32 = 0x7766_5500;

/// Ask the boot chain for a non-persistent bootloader return. This is
/// deliberately kept separate from the diagnostic transport: the command is
/// accepted only in a build with `FULLERENE_AARCH64_DEBUG_RETURN=1`. The
/// Qualcomm IMEM marker is volatile scratch state, not a partition or boot
/// metadata write, and matches the marker used by the standalone probe to
/// request the bootloader/Fastboot path after a reset.
pub(crate) fn return_to_boot_chain() -> ! {
    #[cfg(fullerene_aarch64_bramble)]
    unsafe {
        core::ptr::write_volatile(IMEM_RESTART_REASON as *mut u32, IMEM_BOOTLOADER_REASON);
        core::arch::asm!("dsb sy", "isb", options(nostack));
    }
    unsafe {
        core::arch::asm!(
            "mov w0, #9",
            "movk w0, #0x8400, lsl #16",
            "mov x1, xzr",
            "mov x2, xzr",
            "mov x3, xzr",
            "smc #0",
            out("x0") _,
            out("x1") _,
            out("x2") _,
            out("x3") _,
            options(nostack)
        );
    }
    loop {
        unsafe { core::arch::asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

#[inline]
unsafe fn gadget_mut() -> &'static mut Ep0Simulator {
    // Use a raw pointer for the retained early-boot singleton.  Rust 2024
    // rejects direct references to `static mut`; interrupt/polling access is
    // serialized by the single-core bring-up path.
    unsafe { &mut *addr_of_mut!(GADGET) }
}

#[inline]
unsafe fn gadget_ref() -> &'static Ep0Simulator {
    unsafe { &*addr_of!(GADGET) }
}

#[inline]
unsafe fn udc_mut() -> &'static mut UsbUdc {
    unsafe { &mut *addr_of_mut!(UDC) }
}

/// End the gadget-function lifetime exactly once before requests, endpoint
/// commands, or DMA channels are torn down.
unsafe fn unbind_function() {
    unsafe {
        debug_transport::reset();
        if FUNCTION_BOUND {
            GadgetDriver::on_function_unbind(gadget_mut());
            FUNCTION_BOUND = false;
        }
    }
}

/// Outcome of the previous attempt's last STARTTRANSFER command, harvested
/// from the retained trace at the start of the next handoff attempt (see
/// `harvest_trace_outcome()`). Encoding: 0xFFFF = no record found,
/// 0x8000_0000 | raw DEPCMD register = the command timed out, otherwise the
/// raw DEPCMD register at completion (status bits 15:12, resource index
/// 22:16).
static mut TRACE_HARVEST: u32 = 0xFFFF_FFFF;
/// Raw DEPCMD register of the previous attempt's last SETTRANSFRESOURCE
/// (resource index bits 22:16, status bits 15:12) or 0xFFFF_FFFF.
static mut TRACE_HARVEST_RSC: u32 = 0xFFFF_FFFF;
/// Raw DEPCMD register of the previous attempt's last DEPSTARTCFG.
static mut TRACE_HARVEST_CFG: u32 = 0xFFFF_FFFF;
/// Raw DEPCMD register of the previous attempt's NEWEST STARTTRANSFER (the
/// last one issued before the reset), or 0xFFFF_FFFF.
static mut TRACE_HARVEST_LAST: u32 = 0xFFFF_FFFF;
/// Number of SETUP packets the previous attempt received (trace count of
/// TRACE_SETUP_RECEIVED).
static mut TRACE_HARVEST_SETUP: u32 = 0;
/// Number of descriptor DATA-IN transfers the previous attempt queued (trace
/// count of TRACE_DESCRIPTOR_QUEUED): proves the SETUP was parsed as a real
/// host request and the data phase was dispatched.
static mut TRACE_HARVEST_DESC: u32 = 0;
/// Raw DEPCMD register of the previous attempt's NEWEST STARTTRANSFER on
/// physical endpoint 1 (the data/status IN direction of EP0). A timed-out
/// command carries the 0x8000_0000 flag; bit 16 alone is a healthy
/// XferRscIdx=1 completion, not a timeout.
static mut TRACE_HARVEST_EP1: u32 = 0xFFFF_FFFF;
/// TRB status of the previous attempt's NEWEST XferComplete on physical
/// endpoint 1 (the control data-phase IN), or 0xFFFF_FFFF when the core
/// never completed the data TRB: 0x8 is the healthy LST|IOC completion, any
/// other value names the in-core transfer error.
static mut TRACE_HARVEST_EP1_XFER: u32 = 0xFFFF_FFFF;
/// Number of XferNotReady(CONTROL_DATA) events on physical endpoint 1: the
/// core reports it after fetching the data TRB, before any IN token is
/// answered with data.
static mut TRACE_HARVEST_EP1_NRDY: u32 = 0;
/// Newest post-Run/Stop event-DMA probe result. Bits 0..2 are GETEPSTATE
/// command success, EP0 still armed, and event-ring delivery respectively;
/// bit 16 marks that the retained trace contained a probe record at all.
static mut TRACE_HARVEST_POST: u32 = 0xFFFF_FFFF;
/// Number of STATUS-phase transfers the previous attempt queued (trace count
/// of TRACE_STATUS_QUEUED): proves the DATA phase completed on the wire and
/// the control state machine advanced.
static mut TRACE_HARVEST_STATUSQ: u32 = 0;
/// Number of poll-guard arm successes (TRACE_SETUP_QUEUED with the "ARME"
/// marker) in the previous attempts: proves the guard's deferred Start
/// Transfer ever succeeded while live.
static mut TRACE_HARVEST_ARMED: u32 = 0;
/// Sequence numbers of the OLDEST guard-arm (ARME) and OLDEST SETUP
/// reception: if the arm's sequence is lower, the SETUP TRB was armed before
/// the host's first SETUP token arrived (the arm won the race).
static mut TRACE_HARVEST_ARM_SEQ: u32 = 0xFFFF_FFFF;
static mut TRACE_HARVEST_SETUP_SEQ: u32 = 0xFFFF_FFFF;
/// Seconds between the previous attempt's Connect Done and its first SETUP
/// reception (0xFFFF = no such pair observed).
static mut TRACE_HARVEST_SETUP_DELAY: u32 = 0xFFFF;
/// CNTPCT tick of the last Connect Done, for the SETUP-delay measurement.
static mut CONNECT_TICK: u64 = 0;
/// Number of Connect Done events in the previous attempts: proves the core's
/// link FSM ever came up (without it the core cannot see any host traffic).
static mut TRACE_HARVEST_CONNECT: u32 = 0;
/// Number of SET_ADDRESS (bRequest=5) SETUP packets received: proves the
/// host accepted the device descriptor and moved to the next enumeration
/// stage, i.e. the DATA phase genuinely reached the host.
static mut TRACE_HARVEST_ADDR: u32 = 0;
/// 1 when a GET_DESCRIPTOR arrived AFTER a SET_ADDRESS: the host accepted
/// the address and sent the ADDRESSED read/all request, so the address
/// application worked and the failure is in the addressed response.
static mut TRACE_HARVEST_ADDR2: u32 = 0;
/// Newest "DARM" data-phase arm outcome (bit 16 = a record exists, bit 0 =
/// the Start Transfer ultimately queued after retries) or 0xFFFF_FFFF.
static mut TRACE_HARVEST_DARM: u32 = 0xFFFF_FFFF;
/// Newest SETUP packet: (bRequest << 16) | wLength, or 0xFFFF_FFFF when no
/// SETUP was ever received this boot.
static mut TRACE_HARVEST_LAST_SETUP: u32 = 0xFFFF_FFFF;
static mut INIT_CALLS: u32 = 0;
/// GCTL.RAMCLKSEL observed while the previous owner (Fastboot) still had a
/// working gadget. CSFTRST and the host's bus USB reset both clear this
/// field, and with the wrong select the DWC3 internal RAM misroutes
/// endpoint-context writes, which shows up as STARTTRANSFER failing with
/// "No resource" even though SETTRANSFRESOURCE reported success. Capture
/// the working value and re-apply it at every reset boundary.
static mut RAMCLK_CAPTURE: u32 = 0;
/// Distinguishes a real capture of RAMCLKSEL=0 from the uninitialized state.
static mut RAMCLK_CAPTURE_VALID: bool = false;

#[inline]
fn gctl_ramclksel(gctl: u32) -> u32 {
    (gctl >> 6) & 3
}

/// Retain the raw UTMI-facing state at a handoff boundary. The trace is
/// intentionally split into compact records so it can be inspected later
/// through the existing EP0 trace transport without adding a new control
/// request. `stage` is caller-defined; the high byte on the request word
/// identifies the record group.
unsafe fn trace_utmi_state(stage: u32) {
    unsafe {
        let clocks = super::platform::bramble::usb_clock::read_usb_clock_register_state();
        let gusb2 = read(GUSB2PHYCFG0);
        live_utmi_stage(stage, gusb2);
        trace_event(
            TRACE_UTMI_STATE,
            stage,
            gusb2,
            read_qscratch(QSCRATCH_GENERAL_CFG),
            clocks.utmi_source_config,
            clocks.controller_branches[3],
        );
        trace_event(
            TRACE_UTMI_STATE,
            stage | 0x0100_0000,
            clocks.core_source_config,
            clocks.controller_branches[0],
            clocks.controller_branches[1],
            clocks.controller_branches[2],
        );
        trace_event(
            TRACE_UTMI_STATE,
            stage | 0x0200_0000,
            clocks.controller_branches[4],
            clocks.controller_branches[5],
            read(GUSB3PIPECTL0),
            read(DSTS),
        );
        trace_event(
            TRACE_UTMI_STATE,
            stage | 0x0300_0000,
            read_volatile(hsphy_reg(HSPHY_UTMI_CTRL0)),
            read_volatile(hsphy_reg(HSPHY_CTRL2)),
            read_volatile(hsphy_reg(HSPHY_UTMI_CTRL5)),
            read_qscratch(QSCRATCH_HS_PHY_CTRL),
        );
        trace_event(
            TRACE_UTMI_STATE,
            stage | 0x0800_0000,
            gdb_ltssm_link_state(),
            0,
            0,
            0,
        );
    }
}

/// Refresh the read-only UTMI/DWC3 snapshot immediately before a diagnostic
/// readout publishes it.  The normal stage-5 record is captured at the
/// Run/Stop boundary, before xHCI has necessarily issued its bus reset; a
/// later signal-gate read therefore needs a same-boot sample from the end of
/// the host observation window.
pub fn trace_utmi_state_for_readout() {
    unsafe { trace_utmi_state(5) }
}

#[inline]
fn usb_clock_stable_delay_us() -> u32 {
    option_env!("FULLERENE_USB_CLOCK_STABLE_DELAY_US")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0)
}

/// Architectural counter ticks (CNTPCT_EL0). Firmware always provides the
/// counter frequency on this platform; a zero read simply disables the
/// SETUP-delay measurement.
#[inline]
pub fn arch_counter_ticks() -> u64 {
    arch_counter()
}

/// Public one-second deadline helper for probe readout windows.
#[inline]
pub fn window_deadline_ticks(secs: u64) -> u64 {
    let frequency = arch_counter_frequency();
    if frequency == 0 {
        return u64::MAX;
    }
    arch_counter().saturating_add(frequency.saturating_mul(secs))
}

/// Bounded readout delay that keeps the USB core domain alive.
///
/// The readout sites encode a register value in the delay that follows them,
/// so the delay must not outlive the domain: RPMh collapses the restored USB
/// domain after a few seconds without activity, and a plain spin there makes
/// the following pull-up never become host-visible (run `181751.0` lost its
/// Fullerene attach entirely behind a 15 s plain-spin readout delay). Re-assert
/// the same CX/interconnect/rail votes and the USB30 GDSC that
/// `park_for_seconds()` uses, at the same 0.5 s cadence, and touch nothing else:
/// no reset line, no controller register, no endpoint state.
pub fn readout_keepalive_delay_ms(milliseconds: u64) {
    let frequency = arch_counter_frequency();
    if frequency == 0 {
        return;
    }
    let deadline = arch_counter().saturating_add(frequency.saturating_mul(milliseconds) / 1_000);
    let period = frequency.saturating_div(2);
    let mut next_keepalive = arch_counter().saturating_add(period);
    while arch_counter() < deadline {
        // PET THE WATCHDOG. This helper keeps the USB domain powered across a readout wait,
        // and every caller assumes the wait is survivable - but until 2026-09-28 it did
        // not pet, and `init_usb2_gadget_reuse_fastboot_ep0` (which calls it in more than
        // twenty places, with no `wdt_pet()` of its own anywhere) was killed mid-handoff by
        // the apps watchdog. Measured: `hd_entered` TRUE / `hd_returned` FALSE with
        // `boot-reason=watchdog`. See usb/README.md §1.5.
        watchdog::wdt_pet();
        if arch_counter() >= next_keepalive {
            unsafe {
                let _ = super::platform::bramble::refresh_usb_domain_votes(
                    super::platform::bramble::UsbBusVote::Nominal,
                    true,
                );
                let _ = super::platform::bramble::force_enable_usb30_gdsc();
            }
            next_keepalive = arch_counter().saturating_add(period);
        }
        core::hint::spin_loop();
    }
}

#[inline]
fn arch_counter() -> u64 {
    let value: u64;
    unsafe {
        core::arch::asm!(
            "mrs {value}, CNTPCT_EL0",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[inline]
fn arch_counter_frequency() -> u64 {
    let value: u64;
    unsafe {
        core::arch::asm!(
            "mrs {value}, CNTFRQ_EL0",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

/// Optionally restore the captured GCTL.RAMCLKSEL.
///
/// qpr1's `dwc3_gadget_conndone_interrupt()` explicitly documents that the
/// field is reset to zero after USB reset and that the downstream driver
/// intentionally keeps that reset value. Therefore the source-compatible
/// default is a no-op: the hardware reset value must be allowed to stand.
/// Reapplying the previous owner's value remains available only as a named
/// diagnostic differential for reproducing older Fullerene observations.
unsafe fn reapply_ramclksel() {
    unsafe {
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_reapply_ramclksel) {
            return;
        }
        if !RAMCLK_CAPTURE_VALID {
            return;
        }
        let captured = RAMCLK_CAPTURE;
        let gctl = read(GCTL);
        let updated = (gctl & !(3 << 6)) | (captured << 6);
        if updated != gctl {
            write(GCTL, updated);
            let _ = read(GCTL);
            trace_event(TRACE_DWC3_REVISION_QUIRK, 0x524D_434B, gctl, updated, 0, 0);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ep0State {
    Setup,
    Data,
    Status,
}

/// Clear the linker-reserved DWC3 DMA region before enabling the controller.
///
/// The USB probe enters with caches/MMU disabled, so this is intentionally a
/// volatile byte/word clear rather than a normal Rust slice operation. The
/// caller must invoke it only after the previous controller owner has stopped
/// issuing DMA; it also seeds the allocator for later GSI/UDC allocations.
pub fn clear_dma_memory() {
    let mut current = addr_of!(__usb_dma_start) as usize;
    let end = addr_of!(__usb_dma_end) as usize;
    while current < end {
        unsafe {
            write_volatile(current as *mut u64, 0);
        }
        current += core::mem::size_of::<u64>();
    }
    unsafe {
        let pool = super::platform::bramble::usb_resources().dma_pool;
        let first_free = (end as u64 + 0xfff) & !0xfff;
        DMA_ALLOCATOR = super::platform::bramble::DmaPoolAllocator::new(pool, first_free);
    }
    trace_begin();
}

/// Allocate an identity-mapped USB DMA object from the active DT pool. The
/// caller must invoke this only after the SMMU/CPU mapping for the pool is
/// live; the returned pointer has the same address as the IOVA on Bramble.
pub unsafe fn allocate_usb_dma(size: usize, alignment: usize) -> Option<*mut u8> {
    if size == 0 || alignment == 0 {
        return None;
    }
    unsafe {
        let allocator = &mut *addr_of_mut!(DMA_ALLOCATOR);
        let allocator = allocator.as_mut()?;
        allocator
            .allocate(size as u64, alignment as u64)
            .map(|address| address as usize as *mut u8)
    }
}

/// Return whether the handoff probe has successfully started at least one
/// EP0 DATA or STATUS transfer. This is intentionally weaker than
/// SET_CONFIGURATION: a host may fetch descriptors without configuring the
/// diagnostic gadget, and that must not look like a hung probe.
pub fn probe_ep0_progress() -> bool {
    unsafe { PROBE_EP0_PROGRESS }
}

/// Publish that the normal Bramble entry path owns a live early USB gadget.
/// UFS uses this as a narrow cooperative-polling hook while its bounded
/// controller waits run before the main USB loop is reached.
/// Millisecond timestamp of the *handoff entry*. Provably earlier than the
/// DEVICE-mode write, so `dtimh*` cannot underflow the way `dtimr*` did.
/// Did the deferred-site arm run this boot, and did it report success?
/// Packed: bit0 = called, bit1 = returned true. Published over CCS because
/// `trace_event` writes to the retained `.usb_trace` region, which the harness
/// does not export into the run directory.
pub(crate) static DEFERRED_ARM_RESULT: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

pub(crate) static HANDOFF_TICK_MS: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(u32::MAX);

/// Shared millisecond counter for the timing latches. `u32::MAX` when CNTFRQ is
/// unset, so callers can treat it as "unavailable" rather than as a time.
pub(crate) fn now_ms_for_timing() -> u32 {
    let freq = super::timer::frequency();
    if freq == 0 {
        u32::MAX
    } else {
        (super::timer::counter() / (freq / 1000) as u64) as u32
    }
}

pub fn set_early_handoff_active(active: bool) {
    unsafe {
        EARLY_HANDOFF_ACTIVE = active;
    }
}

/// Mark the bounded Bramble handoff as in progress. This is only a
/// diagnostic escape hatch: the exception path consumes it only in builds
/// explicitly compiled with `FULLERENE_AARCH64_DEBUG_RETURN=1`.
pub fn set_early_handoff_in_progress(active: bool) {
    unsafe {
        EARLY_HANDOFF_IN_PROGRESS = active;
    }
}

/// Retain the architectural state of a synchronous abort before the
/// debug-return path hands control back to the boot chain. The exception
/// handler cannot safely format UART output while a USB MMIO access may have
/// faulted, so keep the compact five-word payload in the existing trace ABI:
/// ESR low/high, FAR low/high, and the low ELR word.
pub(crate) fn trace_sync_exception(esr_el1: u64, far_el1: u64, elr_el1: u64) {
    trace_event(
        TRACE_EXCEPTION_SYNC,
        esr_el1 as u32,
        (esr_el1 >> 32) as u32,
        far_el1 as u32,
        (far_el1 >> 32) as u32,
        elr_el1 as u32,
    );
}

#[inline]
pub(crate) fn early_handoff_in_progress() -> bool {
    unsafe { EARLY_HANDOFF_IN_PROGRESS }
}

#[inline]
pub(crate) fn early_handoff_active() -> bool {
    unsafe { EARLY_HANDOFF_ACTIVE }
}

fn note_probe_ep0_progress() {
    unsafe {
        PROBE_EP0_PROGRESS = true;
    }
}

#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
pub fn gadget_handoff_failure_stage() -> u32 {
    unsafe { GADGET_HANDOFF_FAILURE_STAGE }
}
#[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
pub fn gadget_handoff_failure_stage() -> u32 {
    0
}

#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
pub fn gadget_handoff_stage_probe_enabled() -> bool {
    cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_1)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_2)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_3)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_4)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_5)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_6)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_7)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_8)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_9)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_10)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_11)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_12)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_13)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_14)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_15)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_16)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_17)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_18)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_19)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_20)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_21)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_22)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_23)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_24)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_25)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_26)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_27)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_28)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_29)
}

#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
pub fn gadget_handoff_post_init_stage_probe_enabled() -> bool {
    cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_25)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_26)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_27)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_28)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_29)
}

#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
pub unsafe fn gadget_handoff_post_init_stage_probe(stage: u32) -> bool {
    unsafe { stop_after_gadget_handoff_stage(stage) }
}

#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
fn gadget_handoff_fail(stage: u32) -> bool {
    unsafe {
        GADGET_HANDOFF_FAILURE_STAGE = stage;
    }
    trace_marker(TRACE_PROBE_WATCHDOG, 0x4641_0000 | (stage & 0xff)); // "FA" + stage
    // A selected stage probe must distinguish "the operation reached its
    // boundary" from "the operation failed before the boundary".  For the
    // pre-STARTTRANSFER stages the already-proven bare pull-up is still the
    // correct electrical probe.  Once EP0 has been armed, repeat only the
    // controller-side Run/Stop boundary; re-running the bare initializer
    // would rewrite endpoint/DMA state and hide the actual failure point.
    if gadget_handoff_stop_selected(stage) {
        unsafe {
            if stage >= 6 {
                let _ = stop_after_gadget_handoff_stage(stage);
            } else {
                let _ = init_usb2_bare_pullup_handoff_inner(true);
            }
        }
    }
    false
}

#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
#[inline]
fn gadget_handoff_stop_selected(stage: u32) -> bool {
    match stage {
        1 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_1),
        2 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_2),
        3 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_3),
        4 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_4),
        5 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_5),
        6 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_6),
        7 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_7),
        8 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_8),
        9 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_9),
        10 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_10),
        11 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_11),
        12 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_12),
        13 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_13),
        14 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_14),
        15 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_15),
        16 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_16),
        17 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_17),
        18 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_18),
        19 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_19),
        20 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_20),
        21 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_21),
        22 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_22),
        23 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_23),
        24 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_24),
        25 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_25),
        26 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_26),
        27 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_27),
        28 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_28),
        29 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_29),
        _ => false,
    }
}

/// Publish the physical pull-up at one handoff boundary, then return through
/// the normal failure/recovery path. This is a host-observable stage probe:
/// it deliberately does not pretend that an EP0-less pull-up is a working
/// gadget, but it tells us whether the preceding DWC3 operation still leaves
/// the USB2 electrical path able to attach before the handset recovers.
#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
unsafe fn stop_after_gadget_handoff_stage(stage: u32) -> bool {
    if !gadget_handoff_stop_selected(stage) {
        return false;
    }
    trace_marker(TRACE_PROBE_WATCHDOG, 0x5354_0000 | (stage & 0xff)); // "ST" + stage
    if stage == 7 {
        // Stage 7 is immediately after STARTTRANSFER.  Keep this probe on
        // the exact production boundary: only reassert the Qualcomm session
        // votes, select the USB2 speed, and perform Run/Stop.  Re-running the
        // bare initializer would reset/reconfigure the controller and make
        // a successful STARTTRANSFER indistinguishable from a failed one.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(QSCRATCH_HS_PHY_CTRL, (1 << 20) | (1 << 28));
        configure_gadget_speed(
            cfg!(fullerene_aarch64_usb_dcfg_superspeed)
                || (stage >= 13 && cfg!(fullerene_aarch64_usb_gadget_handoff_super_speed)),
        );
        if !unsafe { run_stop_device(true) } {
            // If STARTTRANSFER completed but the production Run/Stop
            // boundary did not, reset the controller and expose the known
            // electrical probe.  No attach in this stage then points to the
            // STARTTRANSFER boundary itself; an attach points to Run/Stop.
            let _ = unsafe { device_soft_reset() };
            let _ = unsafe { init_usb2_bare_pullup_handoff_inner(true) };
        }
        return true;
    }
    if stage == 8 {
        // STARTTRANSFER may leave the endpoint command engine busy on a
        // failed handoff.  Reset only the DWC3 device state before falling
        // back to the known-good electrical probe, so this failure boundary
        // remains observable even when the command itself wedged the core.
        let _ = unsafe { device_soft_reset() };
        let _ = unsafe { init_usb2_bare_pullup_handoff_inner(true) };
        return true;
    }
    if stage >= 21 {
        // Stages 21-24 are called after the production final Run/Stop has
        // already returned.  Do not issue a second Run/Stop here: that would
        // erase the very boundary being measured.  Capture the live SS state
        // first, then use the known USB2 marker only to make stage reach
        // visible when SS never trains on the host.
        if cfg!(fullerene_aarch64_usb_gadget_handoff_super_speed) {
            capture_ss_state_snapshot();
        }
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if let Some(selector) =
            option_env!("FULLERENE_USB_SIGNAL_CMD_GATE").filter(|value| value.starts_with("ss-"))
        {
            // Stage 21 is the post-production Run/Stop boundary.  The
            // generic SS gate below only adds a sub-second DCTL cycle per
            // set bit, which is not separable in the host's whole-second
            // journal.  Reuse the stage-20 0/4/8-second encoding here, after
            // the live snapshot and before the known USB2 fallback, so the
            // post-Run/Stop register value is actually measurable without
            // changing PHY, DWC3, endpoint, or packet state.
            let code = utmi_readout_code(selector).min(15);
            let delay_ms = if selector == "ss-ltssm-bit1-wide" {
                // The ordinary 4 s bucket landed at 12 s in one host
                // journal, between the observed 0-bit and 1-bit clusters.
                // This diagnostic keeps the captured bit unchanged but gives
                // a 1-bit result an 8 s publication delay, making it
                // separable from attach jitter while staying below recovery.
                match code {
                    0 => 0,
                    1 => 8_000,
                    _ => 8_000,
                }
            } else if selector == "ss-ltssm" {
                u64::from(code.saturating_mul(250).min(4_000))
            } else {
                match code {
                    0 => 0,
                    1 => 4_000,
                    _ => 8_000,
                }
            };
            crate::timer::delay_ms(delay_ms);
        }
        let _ = super::platform::bramble::rearm_usb2_android_clock_branches();
        let _ = super::platform::bramble::enable_usb2_utmi_clock();
        let _ = unsafe { init_usb2_bare_pullup_handoff_inner(true) };
        return true;
    }
    if stage >= 6 {
        // At this point the real handoff path has already performed the
        // controller-side PHY/clock setup and, for stage 6, queued the first
        // EP0 STARTTRANSFER. Re-running the bare initializer would rewrite
        // those stateful registers and make the stage probe test a different
        // path from the actual Run/Stop boundary.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(QSCRATCH_HS_PHY_CTRL, (1 << 20) | (1 << 28));
        if stage >= 13 && cfg!(fullerene_aarch64_usb_gadget_handoff_super_speed) {
            // The source-ordered QMP handoff asserts USB3 PIPE SUSPHY before
            // PHY init. The full path clears it before endpoint publication;
            // this minimal post-QMP link probe must do the same before its
            // first SS Run/Stop transition or it tests a deliberately
            // suspended PIPE rather than QMP/link bring-up.
            let mut usb3 = read(GUSB3PIPECTL0);
            usb3 &= !GUSB3PIPECTL_SUSPHY;
            write(GUSB3PIPECTL0, usb3);
            let _ = read(GUSB3PIPECTL0);
        }
        configure_gadget_speed(
            cfg!(fullerene_aarch64_usb_dcfg_superspeed)
                || (stage >= 13 && cfg!(fullerene_aarch64_usb_gadget_handoff_super_speed)),
        );
        let _ = unsafe { run_stop_device(true) };
        if stage >= 13 && stage != 20 && cfg!(fullerene_aarch64_usb_gadget_handoff_super_speed) {
            // Capture before returning to the host-visible probe. The host
            // may issue reset/descriptor traffic immediately after attach;
            // a later live read would no longer describe this boundary.
            capture_ss_state_snapshot();
        }
        if stage >= 15 {
            // The SS Run/Stop probe itself is not host-visible when the link
            // never trains. For the post-stage-14 bisection, convert a
            // reached boundary into the already-proven USB2 attach marker
            // after the SS snapshot has been taken. This does not claim that
            // USB2 enumeration works; it only makes stage reach observable.
            // A preceding SS-only Fastboot session may have gated the mock
            // UTMI branch, so restore that one clock before asking DWC3 for
            // the USB2 marker. This is diagnostic recovery, not part of the
            // stage boundary being measured.
            let _ = super::platform::bramble::rearm_usb2_android_clock_branches();
            let _ = super::platform::bramble::enable_usb2_utmi_clock();
            let _ = unsafe { init_usb2_bare_pullup_handoff_inner(true) };
        }
        return true;
    }
    // Reuse the exact bare path already proven to create a physical attach.
    // This keeps the stage experiment about the preceding handoff boundary,
    // rather than introducing a second, subtly different Run/Stop sequence.
    let _ = unsafe { init_usb2_bare_pullup_handoff_inner(true) };
    true
}

/// Initialize the Bramble DWC3 in peripheral mode and connect the pull-up.
pub fn init() -> bool {
    super_speed::init_with_super_speed(true, true, true)
}

/// Initialize only the USB2 path for the dependency-free hardware probe.
pub fn init_usb2_only() -> bool {
    super_speed::init_with_super_speed(false, true, true)
}

/// Publish a 4-bit word from a live controller register as pull-up pulses.
///
/// This is the readout that the attach-latency carriers could not provide: a
/// post-Run/Stop word needs a channel that is still observable after the
/// pull-up has risen, and the host-side CCS bit is exactly that - the root
/// hub's own class `GetPortStatus` traffic reports it every time it changes
/// (see `tools/bramble_port_ccs.py`). `RUN_STOP` reaches the PHY only on the
/// `run_stop_device_no_readback` path, i.e. in gate runs, so this publisher is
/// a no-op unless the run used `--signal-cmd-gate`.
///
/// Encoding: a 1000 ms marker pulse, then one pulse per bit with a width that
/// carries the value - bit b set is 200 + 200*b ms (200/400/600/800), bit b
/// clear is a 50 ms blip. Five pulses fit in the ~5.7 s window with margin.
///
/// The pulse uses `run_stop_device`, not `run_stop_device_no_readback`, and
/// that is not incidental: `prepare_run_stop_device` saves and clears
/// `GUSB2PHYCFG0.SUSPHY | ENBLSLPM` and both callers restore them afterwards,
/// so the no-readback variant re-suspends the PHY immediately after the
/// `DCTL` write. A start takes effect before that restore (the handoff's own
/// pull-up rises this way), but a stop needs the halt handshake to run first -
/// which is exactly what `wait_device_state` provides. Run `232050.0` proved
/// the difference: the same publisher emitted no pulse at all through the
/// no-readback variant, while the blip branch, which uses `run_stop_device`,
/// produced clean pulses in `219752.0` / `221600.0`.
unsafe fn ccs_pulse(stop_ms: u64) {
    let _ = unsafe { run_stop_device(false) };
    readout_keepalive_delay_ms(stop_ms);
    let _ = unsafe { run_stop_device(true) };
    readout_keepalive_delay_ms(200);
}

/// Publish ONE predicate as ONE pulse. **This is the only readout encoding.**
///
/// The channel is DCTL Run/Stop cycling: every pulse makes the host print one
/// more `new high-speed USB device` line, so
///
/// ```text
/// attach lines - 1 (the boot's own attach) = the number of TRUE predicates,
///                                            in source order
/// ```
///
/// is the whole decoder, and it is the *same* decoder for every selector.
///
/// This function exists because the previous encoding was not uniform, and that
/// is what made this project assign a meaning to a measurement and then retract
/// it. Of the 49 readout selectors measured on 2026-09-26, 24 used a counted
/// number of pulses, 18 used width-coded pulses, 7 emitted none, and **none**
/// used the one-predicate-one-pulse form. Each encoding had to be decoded by
/// reading its own implementation, so an off-by-one detail in that reading
/// produced a confident wrong answer twice for SOFFN alone. Two specific
/// hazards, both removed here:
///
/// * a leading `ccs_pulse(1_000)` **marker** made the count ambiguous - one
///   boot attach plus two counted pulses and one boot attach plus a marker plus
///   one counted pulse are the *same three lines* on the host;
/// * width codes drift (`200/400/600/800 ms` measured as `270/415/416/789`).
///
/// So: fixed 300 ms, no marker, one call per predicate, order = source order.
/// Never add a second encoding; if a predicate needs transmitting, give it its
/// own `readout_bit()` call in the order it should be read.
unsafe fn readout_bit(truth: bool) {
    if truth {
        // MUST use the no-readback gate. The post-Run/Stop readout block
        // (`mod.rs:7298-8724`) runs after the host has started enumerating, and
        // the plain `ccs_pulse` waits on the Run/Stop halt readback - which is
        // exactly what stops pulses reaching the host from that point on
        // (runs `336520.0` and `350817.0`: no pulses at all in the CCS
        // timeline, while the same channel works fine before the attach).
        // `ccs_pulse_no_readback` is the variant the source says to use
        // "for any readout that must run at attach time".
        unsafe { ccs_pulse_no_readback(300) };
    }
}

/// The same CCS pulse, but through the *no-readback* Run/Stop gate.
///
/// `ep0_signal_drop_pullup()` records the reason this exists
/// (`usb.rs:12011-12017`): on Bramble the host-visible pull-up is owned by DCTL
/// Run/Stop, not by the Qualcomm session/VBUS glue, and
/// `run_stop_device_no_readback` is "the proven host-visible gate" - it
/// deliberately skips the halt readback so a wedged or busy core cannot hide the
/// transition. The plain `ccs_pulse` above waits on that readback (up to 2 s per
/// call), which is why pulses issued *after* the host has started enumerating
/// never reached the host (runs `336520.0` and `350817.0`: no pulses at all in
/// the CCS timeline, while the same channel works fine before the attach). Use
/// this variant for any readout that must run at attach time.
unsafe fn ccs_pulse_no_readback(stop_ms: u64) {
    let _ = unsafe { run_stop_device_no_readback(false) };
    readout_keepalive_delay_ms(stop_ms);
    let _ = unsafe { run_stop_device_no_readback(true) };
    readout_keepalive_delay_ms(200);
}

/// Publish a small count as *host-visible attach lines* by cycling DCTL Run/Stop.
///
/// This is the only readout channel that works after the attach. The reason is
/// recorded in `usb_probe.rs:1331-1372`: the QSCRATCH, DCTL and VBUSVLDEXT0
/// pull-up *drop* primitives are electrically inert on this revision, so
/// `ccs_pulse`-style pulses are invisible - but "DCTL Run/Stop is the one
/// disconnect primitive the host actually sees", and each stop/run cycle makes
/// the host print one "new high-speed USB device" line. The host-side decoder is
/// therefore: attach-line count minus the first attach equals the published
/// count.
///
/// The handset also self-resets ~5.5-8 s after the attach, so the cycles have to
/// happen before that; ~8 s after the handoff is still inside the window.
unsafe fn gate_cycle_publish(count: u32) {
    for _ in 0..count.min(8) {
        let _ = unsafe { gate_true_stop_device() };
        readout_keepalive_delay_ms(250);
        let _ = unsafe { gate_true_run_device() };
        readout_keepalive_delay_ms(300);
    }
}

/// Publish a 4-bit word as four predicates, in bit order, with the uniform
/// encoding: bit 0 first, then bit 1, and so on, each TRUE bit contributing one
/// 300 ms pulse and each FALSE bit contributing nothing.
///
/// The decoder is the same one every other selector uses:
/// `attach lines - 1 = number of TRUE bits`, read in order.
///
/// This used to emit a `1_000` ms marker followed by *width-coded* pulses
/// (`200 + 200 * bit` for a set bit, `50` ms for a clear one). Two reasons it
/// went: the marker made the total attach count ambiguous, and the widths are
/// documented to drift (`200/400/600/800 ms` measured as `270/415/416/789`), so
/// the decoder had to guess. The information carried is unchanged - four bits,
/// in a fixed order - only the delivery is now the common one.
unsafe fn publish_ccs_word(code: u32) {
    for bit in 0..4u32 {
        unsafe { readout_bit((code >> bit) & 1 != 0) };
    }
}

/// Eight-bit variant of [`publish_ccs_word`], for values that do not fit four
/// bits (the writer-attribution line number). Same encoding rule - one pulse per
/// SET bit, bit order from bit 0 - just twice as wide, so the decoder counts
/// pulses the same way. Kept separate so the four-bit path is untouched.
unsafe fn publish_ccs_word_byte(code: u32) {
    for bit in 0..8u32 {
        unsafe { readout_bit((code >> bit) & 1 != 0) };
    }
}

/// One-shot "did the core receive anything at all" readout.
///
/// The host-side CCS bit (the root hub's class `GetPortStatus`, decoded by
/// `tools/bramble_port_ccs.py`) is a working device-to-host channel whenever
/// the handoff took the `run_stop_device_no_readback` path - i.e. in gate runs.
/// Dropping the pull-up once, at the first consumed device event, therefore
/// publishes a single bit that no register readout can: the core saw host
/// traffic (USB Reset, Connect Done, the first SETUP) or it saw nothing.
/// Selected with `--utmi-postrun-readout usb2-live-eventdrop`; it is a no-op
/// for every other selector, so it cannot perturb an existing run.
static mut EVENT_DROP_DONE: bool = false;
/// Set by the `usb2-live-susphy-active` selector once it has cleared
/// `GUSB2PHYCFG0.SUSPHY`, so the same run also reports whether any device event
/// arrives *after* the PHY is no longer suspended. That combination is the one
/// measurement the earlier eventdrop runs could not make: `224475.0` and
/// `225813.0` both ran with the PHY still suspended, so their "zero events"
/// result could not distinguish "the PHY blocks the data path" from "the core
/// never receives anything".
static mut EVENT_DROP_ARMED: bool = false;

unsafe fn publish_first_event_drop() {
    unsafe {
        if !EVENT_DROP_ARMED
            && option_env!("FULLERENE_USB_UTMI_POSTRUN_READOUT") != Some("usb2-live-eventdrop")
        {
            return;
        }
        if EVENT_DROP_DONE {
            return;
        }
        EVENT_DROP_DONE = true;
        // Only the no-readback Run/Stop reaches the PHY's pull-up switch (see
        // the gate-path note on `run_stop_device`), and the drop must be long
        // enough for the root hub's status URB to complete on the host.
        ccs_pulse(300);
    }
}

unsafe fn poll_ep0_event_ring() -> bool {
    if cfg!(fullerene_aarch64_usb_abl_event_consume) {
        return poll_ep0_event_ring_abl_style();
    }
    // STEP BREADCRUMBS (level 5). One once-only pulse after each controller access inside the
    // event-ring drain, so a single run says which access does not return. `try_arm_setup()` did
    // controller MMIO successfully microseconds earlier (bc3!(1) fired), so the clock had not
    // collapsed yet - the stall is at a specific access, not a dead aperture. See usb/README.md §3.24.
    macro_rules! bc5 {
        ($i:expr) => {
            if matches!(
                option_env!("FULLERENE_USB_PULSE_BREADCRUMB"),
                Some("5") | Some("4")
            ) && unsafe { !trace::BC_ONCE[$i] }
            {
                unsafe {
                    trace::BC_ONCE[$i] = true;
                    ccs_pulse(300);
                }
            }
        };
    }
    // DISCRIMINATOR (level 7): is the aperture dead, or is this one register special?
    // An ordinary controller read is performed immediately before the event-count read, with a
    // pulse after each. `bc3!(1)` proved `try_arm_setup()` did controller MMIO and returned, so if
    // even this DCTL read fails to complete then the clock branch died between two adjacent calls;
    // if DCTL completes and GEVNTCOUNT0 does not, the stall is specific to that access.
    // See usb/README.md §3.25.
    let bc7_dctl = unsafe { read(DCTL) };
    if matches!(
        option_env!("FULLERENE_USB_PULSE_BREADCRUMB"),
        Some("7") | Some("4")
    ) && unsafe { !trace::BC_ONCE[6] }
    {
        unsafe {
            trace::BC_ONCE[6] = true;
            ccs_pulse(300);
        }
    }
    let _ = bc7_dctl;
    let count_register = unsafe { read(GEVNTCOUNT0) };
    if matches!(
        option_env!("FULLERENE_USB_PULSE_BREADCRUMB"),
        Some("7") | Some("4")
    ) && unsafe { !trace::BC_ONCE[7] }
    {
        unsafe {
            trace::BC_ONCE[7] = true;
            ccs_pulse(300);
        }
    }
    bc5!(0); // read(GEVNTCOUNT0) returned with count != 0
    let count = count_register & GEVNTCOUNT_MASK;
    if count == 0 {
        return false;
    }
    // Linux masks the event interrupt while the current ring contents are
    // consumed. This matters for the early IRQ path as well as polling: an
    // event posted during process_event() must not re-enter the same consumer
    // before its cursor and acknowledgement are updated.
    let event_base = unsafe { ep0_event_dma_base() };
    let event_size = unsafe { ep0_event_size() };
    unsafe {
        write(
            GEVNTSIZ0,
            GEVNTSIZ_INTMASK | (event_size as u32 & GEVNTSIZ_SIZE_MASK),
        );
    }
    bc5!(1); // write(GEVNTSIZ0) returned
    // Snapshot the producer-owned ring before acknowledging it. This is the
    // same ownership transition as Linux's evt->cache copy in
    // dwc3_check_event_buf(); process_event() must consume this stable copy.
    let start_offset = unsafe { EVENT_OFFSET };
    // Capture the producer/consumer boundary before the ACK below. This is
    // intentionally independent of process_event(): a host-side -71 can be
    // caused by the controller producing no event at all, by a DMA write that
    // never reaches the ring, or by software consuming the event incorrectly.
    // The first observation keeps those cases distinguishable in the retained
    // trace without changing the live event handling.
    let first_offset = start_offset % event_size;
    unsafe { cache_invalidate(event_base + first_offset, 4) };
    bc5!(2); // cache_invalidate returned
    let first_event = (event_base as *const u8).wrapping_add(first_offset);
    let first_word = unsafe {
        u32::from_le_bytes([
            read_volatile(first_event),
            read_volatile(first_event.add(1)),
            read_volatile(first_event.add(2)),
            read_volatile(first_event.add(3)),
        ])
    };
    bc5!(3); // the four read_volatile() event bytes returned
    let first_dsts = unsafe { read(DSTS) };
    bc5!(4); // read(DSTS) returned
    let first_dctl = unsafe { read(DCTL) };
    if trace::live_dwc3_first_event(
        count_register,
        first_offset as u32,
        first_word,
        first_dsts,
        first_dctl,
    ) {
        trace_event(
            TRACE_EVENT_RING_READY,
            0x4645_5630, // "FEV0": first event count/offset/word/DSTS
            count_register,
            first_offset as u32,
            first_word,
            first_dsts,
        );
        trace_event(
            TRACE_EVENT_RING_READY,
            0x4645_5631, // "FEV1": DCTL at the same producer boundary
            first_dctl,
            0,
            0,
            0,
        );
    }
    let mut copied = 0usize;
    while copied < count as usize {
        let offset = (start_offset + copied) % event_size;
        unsafe { cache_invalidate(event_base + offset, 4) };
        let event = (event_base as *const u8).wrapping_add(offset);
        let raw = unsafe {
            u32::from_le_bytes([
                read_volatile(event),
                read_volatile(event.add(1)),
                read_volatile(event.add(2)),
                read_volatile(event.add(3)),
            ])
        };
        unsafe {
            write_volatile(
                addr_of_mut!(EVENT_CACHE.0)
                    .cast::<u32>()
                    .add(copied / core::mem::size_of::<u32>()),
                raw,
            );
        }
        copied += 4;
    }
    unsafe {
        SIGNAL_EVENT_DELIVERED = true;
        EVENT_OFFSET = (start_offset + count as usize) % event_size;
        // Runtime event consumption acknowledges only the byte count. Linux
        // reserves the full-register write (including EHB) for event-buffer
        // setup/cleanup; its interrupt path writes the masked count here and
        // handles EHB separately only when IMOD is enabled.
        write(GEVNTCOUNT0, count);
        core::arch::asm!("dsb sy", options(nostack));
        // Publish the acknowledgement before unmasking, matching the Linux
        // event-buffer handler's ordering. Linux's threaded handler consumes
        // the stable cache before it unmasks the interrupt; keep that same
        // ownership boundary here so process_event() cannot race a newly
        // posted event while it issues the next EP0 command.
    }
    let mut remaining = count as usize;
    let mut cached_offset = 0usize;
    while remaining >= 4 {
        let raw = unsafe {
            read_volatile(
                addr_of!(EVENT_CACHE.0)
                    .cast::<u32>()
                    .add(cached_offset / core::mem::size_of::<u32>()),
            )
        };
        unsafe { process_event(raw) };
        cached_offset += 4;
        remaining -= 4;
    }
    unsafe {
        write(GEVNTSIZ0, event_size as u32 & GEVNTSIZ_SIZE_MASK);
        core::arch::asm!("dsb sy", options(nostack));
    }
    unsafe { publish_first_event_drop() };
    true
}

/// Consume EP0 events in the same ownership order as Factory ABL.
///
/// The stock event loop reads one 32-bit event, dispatches it while the
/// event is still owned by the software consumer, advances the ring cursor,
/// and then writes exactly four consumed bytes back to GEVNTCOUNT while
/// preserving EHB.  The normal Fullerene path snapshots a whole available
/// batch before ACKing it; keep this source-derived sequence as a narrow A/B
/// because EP0 event handling can issue the next endpoint command.
unsafe fn poll_ep0_event_ring_abl_style() -> bool {
    let count_register = unsafe { read(GEVNTCOUNT0) };
    let count = count_register & GEVNTCOUNT_MASK;
    if count == 0 {
        return false;
    }
    let event_base = unsafe { ep0_event_dma_base() };
    let event_size = unsafe { ep0_event_size() };
    let mut consumed = 0usize;
    while consumed < count as usize {
        let offset = unsafe { EVENT_OFFSET } % event_size;
        unsafe { cache_invalidate(event_base + offset, 4) };
        let event = (event_base as *const u8).wrapping_add(offset);
        let raw = unsafe {
            u32::from_le_bytes([
                read_volatile(event),
                read_volatile(event.add(1)),
                read_volatile(event.add(2)),
                read_volatile(event.add(3)),
            ])
        };
        unsafe {
            SIGNAL_EVENT_DELIVERED = true;
            let first_dsts = read(DSTS);
            let first_dctl = read(DCTL);
            if trace::live_dwc3_first_event(
                count_register,
                offset as u32,
                raw,
                first_dsts,
                first_dctl,
            ) {
                trace_event(
                    TRACE_EVENT_RING_READY,
                    0x4645_5630, // "FEV0"
                    count_register,
                    offset as u32,
                    raw,
                    first_dsts,
                );
                trace_event(
                    TRACE_EVENT_RING_READY,
                    0x4645_5631, // "FEV1"
                    first_dctl,
                    0,
                    0,
                    0,
                );
            }
            // ABL dispatches before releasing this event back to DWC3. This
            // matters when a SETUP/XferNotReady handler submits the next
            // endpoint command from inside process_event().
            process_event(raw);
            EVENT_OFFSET = (offset + 4) % event_size;
            let current = read(GEVNTCOUNT0);
            write(GEVNTCOUNT0, (current & GEVNTCOUNT_EHB) | 4);
            core::arch::asm!("dsb sy", options(nostack));
        }
        consumed += 4;
    }
    true
}
