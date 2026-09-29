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
    apply_usb31_gadget_reference_deltas, configure_android_hs_connect_done_policy,
    configure_dwc3_device_mode, configure_dwc3_global_control, configure_gadget_speed,
    DEVICE_MODE_WRITE_PROBE,
    configure_gadget_start_defaults, configure_usb2_phy_interface, configure_usb31_lfps_exit_timer,
    configure_usb31_phy_setup, configure_usb31_phy_setup_pre_reset, enable_gadget_susphy,
    enable_usb2_gadget_susphy, qscratch_set, run_stop_value,
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
        let fill = if slot % 2 == 0 { 0xffff_ffff } else { 0x0000_0000 };
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
    let deadline =
        arch_counter().saturating_add(frequency.saturating_mul(milliseconds) / 1_000);
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

/// Scan the retained trace backwards for the last STARTTRANSFER command
/// outcome. Called at the start of every handoff attempt except the first:
/// attempt N therefore reads attempt N-1's records, which are still intact
/// because the trace survives the in-boot DMA-region clear.
unsafe fn harvest_trace_outcome() {
    unsafe {
        let magic = read_volatile(addr_of!(USB_TRACE).cast::<u32>());
        let version = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(1));
        if magic != USB_TRACE_MAGIC || version != USB_TRACE_VERSION {
            return;
        }
        let head = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(2)) as usize;
        if head == 0 {
            return;
        }
        let count = head.min(USB_TRACE_CAPACITY);
        TRACE_HARVEST_SETUP = 0;
        TRACE_HARVEST_DESC = 0;
        TRACE_HARVEST_STATUSQ = 0;
        TRACE_HARVEST_ARMED = 0;
        TRACE_HARVEST_ARM_SEQ = 0xFFFF_FFFF;
        TRACE_HARVEST_SETUP_SEQ = 0xFFFF_FFFF;
        TRACE_HARVEST_CONNECT = 0;
        TRACE_HARVEST_ADDR = 0;
        TRACE_HARVEST_ADDR2 = 0;
        TRACE_HARVEST_DARM = 0xFFFF_FFFF;
        TRACE_HARVEST_LAST_SETUP = 0xFFFF_FFFF;
        TRACE_HARVEST_EP1_XFER = 0xFFFF_FFFF;
        TRACE_HARVEST_EP1_NRDY = 0;
        TRACE_HARVEST_POST = 0xFFFF_FFFF;
        for offset in 0..count {
            let slot = (head.wrapping_sub(1 + offset)) % USB_TRACE_CAPACITY;
            let entry = addr_of!(USB_TRACE.entries)
                .cast::<UsbTraceEntry>()
                .add(slot);
            let event = read_volatile(addr_of!((*entry).event));
            // Count every SETUP the previous attempts received: any count
            // above zero proves the core delivered a SETUP packet to DRAM.
            if event == TRACE_SETUP_RECEIVED {
                TRACE_HARVEST_SETUP = TRACE_HARVEST_SETUP.wrapping_add(1);
            }
            if event == TRACE_DEVICE_CONNECT {
                TRACE_HARVEST_CONNECT = TRACE_HARVEST_CONNECT.wrapping_add(1);
            }
            if event == TRACE_SETUP_RECEIVED {
                let request = read_volatile(addr_of!((*entry).request));
                if request == 5 {
                    TRACE_HARVEST_ADDR = TRACE_HARVEST_ADDR.wrapping_add(1);
                } else if request == 6 && TRACE_HARVEST_ADDR == 0 {
                    // Backward scan: a GET_DESCRIPTOR encountered BEFORE any
                    // SET_ADDRESS record is NEWER than every SET_ADDRESS,
                    // i.e. the host's post-address read/all request.
                    TRACE_HARVEST_ADDR2 = 1;
                }
                // First hit of the backward scan is the newest SETUP.
                if TRACE_HARVEST_LAST_SETUP == 0xFFFF_FFFF {
                    TRACE_HARVEST_LAST_SETUP =
                        (request << 16) | (read_volatile(addr_of!((*entry).length)) & 0xffff);
                }
            }
            if event == TRACE_DESCRIPTOR_QUEUED {
                TRACE_HARVEST_DESC = TRACE_HARVEST_DESC.wrapping_add(1);
                // The "DARM" record carries the final data-phase arm outcome
                // (bit 0 = queued after retries); the backward scan makes the
                // first hit the newest arm.
                if TRACE_HARVEST_DARM == 0xFFFF_FFFF
                    && read_volatile(addr_of!((*entry).request)) == 0x4441_524D
                {
                    TRACE_HARVEST_DARM = 0x1_0000 | (read_volatile(addr_of!((*entry).value)) & 1);
                }
            }
            if event == TRACE_STATUS_QUEUED {
                TRACE_HARVEST_STATUSQ = TRACE_HARVEST_STATUSQ.wrapping_add(1);
            }
            if event == TRACE_TRANSFER_COMPLETE {
                // The dispatch writes request=event kind (1), value=endpoint,
                // index=TRB status. The backward scan makes the first EP1 hit
                // the newest data-phase completion.
                if read_volatile(addr_of!((*entry).request)) == 1
                    && read_volatile(addr_of!((*entry).value)) == 1
                    && TRACE_HARVEST_EP1_XFER == 0xFFFF_FFFF
                {
                    TRACE_HARVEST_EP1_XFER = read_volatile(addr_of!((*entry).index));
                }
            }
            if event == TRACE_XFER_NOT_READY {
                // Recorded as request=endpoint, value=status (1 = CONTROL_DATA,
                // 2 = CONTROL_STATUS).
                if read_volatile(addr_of!((*entry).request)) == 1
                    && read_volatile(addr_of!((*entry).value)) == 1
                {
                    TRACE_HARVEST_EP1_NRDY = TRACE_HARVEST_EP1_NRDY.wrapping_add(1);
                }
            }
            if event == TRACE_EVENT_RING_READY
                && read_volatile(addr_of!((*entry).request)) == 0x504F_5354
                && TRACE_HARVEST_POST == 0xFFFF_FFFF
            {
                // The post probe stores command_ok/ep0_armed in value/index
                // and the first event word in length. A nonzero event word is
                // the observed delivery marker; the record itself lets the
                // gate distinguish "probe not compiled/reached" from a
                // probe that ran and failed one of its checks.
                let command_ok = read_volatile(addr_of!((*entry).value)) & 1;
                let ep0_armed = read_volatile(addr_of!((*entry).index)) & 1;
                let delivered = (read_volatile(addr_of!((*entry).length)) >> 31) & 1;
                TRACE_HARVEST_POST = 0x1_0000 | command_ok | (ep0_armed << 1) | (delivered << 2);
            }
            if event == TRACE_SETUP_QUEUED {
                let marker = read_volatile(addr_of!((*entry).request));
                if marker == 0x4152_4D45 {
                    TRACE_HARVEST_ARMED = TRACE_HARVEST_ARMED.wrapping_add(1);
                    let sequence = read_volatile(addr_of!((*entry).sequence));
                    if sequence < TRACE_HARVEST_ARM_SEQ {
                        TRACE_HARVEST_ARM_SEQ = sequence;
                    }
                }
            }
            if event == TRACE_SETUP_RECEIVED {
                let sequence = read_volatile(addr_of!((*entry).sequence));
                if sequence < TRACE_HARVEST_SETUP_SEQ {
                    TRACE_HARVEST_SETUP_SEQ = sequence;
                }
            }
            if event != TRACE_EP_COMMAND_DONE && event != TRACE_EP_COMMAND_TIMEOUT {
                continue;
            }
            let command = read_volatile(addr_of!((*entry).request)) & 0x0f;
            let raw = read_volatile(addr_of!((*entry).index));
            let command_endpoint = read_volatile(addr_of!((*entry).value));
            let encode = |timeout: bool| -> u32 {
                if timeout {
                    // The timeout flag must not collide with the returned
                    // XferRscIdx (DEPCMD bits 22:16): physical EP1's healthy
                    // resource index is 1, so a completed EP1 STARTTRANSFER
                    // sets bit 16 and a bit-16 flag misreads it as a wedge.
                    0x8000_0000 | raw
                } else {
                    raw & 0x7f_ffff
                }
            };
            let timed_out = event == TRACE_EP_COMMAND_TIMEOUT;
            // The backward scan overwrites: each field ends up holding the
            // chronologically FIRST record of its command type (attempt 1's
            // ep0-out command). The newest STARTTRANSFER values are captured
            // on the first hit before any overwrite can touch them.
            match command {
                DEPCMD_STARTTRANSFER => {
                    if TRACE_HARVEST_LAST == 0xFFFF_FFFF {
                        TRACE_HARVEST_LAST = encode(timed_out);
                    }
                    if command_endpoint == 1 && TRACE_HARVEST_EP1 == 0xFFFF_FFFF {
                        TRACE_HARVEST_EP1 = encode(timed_out);
                    }
                    TRACE_HARVEST = encode(timed_out);
                }
                DEPCMD_SETTRANSFRESOURCE => {
                    TRACE_HARVEST_RSC = encode(timed_out);
                }
                DEPCMD_DEPSTARTCFG => {
                    TRACE_HARVEST_CFG = encode(timed_out);
                }
                _ => {}
            }
        }
        // A SET_ADDRESS received after the newest GET_DESCRIPTOR invalidates
        // the read-all detection (that descriptor read was the pre-address
        // probe, not the post-address read/all).
        if TRACE_HARVEST_ADDR == 0 {
            TRACE_HARVEST_ADDR2 = 0;
        }
    }
}

/// Composite "diag" gate readout: re-harvest THIS run's live trace and fold
/// the enumeration progress into a 1..9 code. The host is a 6.8 xHCI kernel
/// using the "new scheme", so the FIRST control transfer is already the
/// 64-byte device-descriptor read and every SETUP below is one of its two
/// attempts (there is no 8-byte first read):
///   1 = no SETUP ever reached EP0 (event ring / OUT path / CPU hung)
///   2 = only the first SETUP (read/64 attempt 1)
///   3 = both attempts dispatched but the newest queued no DataIn (on_setup
///       stalled or answered with a non-data action)
///   4 = the read/64 data phase was queued but its Start Transfer failed
///   5 = the read/64 data phase armed cleanly but the core never fetched
///       the data TRB
///   6 = the core fetched the read/64 data TRB; the IN token went
///       unanswered (link / core DMA / host side)
///   7 = an EP1 transfer-complete event arrived after the arm (the data
///       phase left the core; status 0 = the host should have the bytes)
///   8 = no completion, but the data TRB's HWO was cleared over DMA: the
///       core consumed the TRB and the transfer silently died (FIFO/PHY)
///   9 = no completion and HWO still set: the core never consumed the
///       armed TRB (the doorbell/resource was lost after the command
///       retired)
///  10 = a completion happened AND the control status was queued
///       (TRACE_STATUS_QUEUED >= 1): the SET_ADDRESS status arm ran, so a
///       failure past this point is wire-level, not arm-machinery
/// DESC counts every TRACE_DESCRIPTOR_QUEUED entry, and each DataIn arm
/// writes TWO (the descriptor record plus the "DARM" marker), so two arms
/// (read/64 attempts 1 + 2) give DESC >= 4. DARM holds the NEWEST arm
/// outcome, which is the newest read/64 arm once DESC >= 4. EP1_NRDY counts
/// the XferNotReady(CONTROL_DATA) events on the data endpoint, one per
/// attempt.
pub fn diag_readout_code() -> u32 {
    unsafe {
        harvest_trace_outcome();
        let mut code = 1u32;
        if TRACE_HARVEST_SETUP > 0 {
            code = 2;
            if TRACE_HARVEST_SETUP >= 2 {
                code = 3;
                if TRACE_HARVEST_DESC >= 4 {
                    code = 4;
                    if TRACE_HARVEST_DARM == 0x1_0001 {
                        code = 5;
                        if TRACE_HARVEST_EP1_NRDY >= 2 {
                            code = 6;
                            if TRACE_HARVEST_EP1_XFER != 0xFFFF_FFFF {
                                code = 7;
                                if TRACE_HARVEST_STATUSQ >= 1 {
                                    code = 10;
                                }
                            } else {
                                // Post-mortem on the data TRB the arm
                                // queued: the core clears HWO over DMA
                                // when it consumes the TRB, so a still-set
                                // HWO means the transfer never left the
                                // doorbell. Invalidate first: the writeback
                                // is the core's, not ours.
                                let trb = ep0_trb_ptr(ep0_trb_index(1));
                                cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
                                let ctrl = read_volatile(addr_of!((*trb).ctrl));
                                if ctrl & TRB_HWO == 0 {
                                    code = 8;
                                } else {
                                    code = 9;
                                }
                            }
                        }
                    }
                }
            }
        }
        code
    }
}

/// Return a host-readable nibble from the newest retained SS snapshot.
/// The probe uses this only for a timing-channel diagnostic; it does not alter
/// the USB controller or clock state.
unsafe fn capture_ss_state_snapshot() {
    unsafe {
        SS_STATE_SNAPSHOT_DSTS = read(DSTS);
        SS_STATE_SNAPSHOT_DCTL = read(DCTL);
        SS_STATE_SNAPSHOT_PIPE = read(GUSB3PIPECTL0);
        SS_STATE_SNAPSHOT_GCTL = read(GCTL);
        SS_STATE_SNAPSHOT_QSCRATCH = read_qscratch(QSCRATCH_SS_PHY_CTRL);
        SS_STATE_SNAPSHOT_QSCRATCH_GENERAL = read_qscratch(QSCRATCH_GENERAL_CFG);
        SS_STATE_SNAPSHOT_QMP = phy::qmp_post_runstop_snapshot();
        SS_STATE_SNAPSHOT_QMP_STATUS2 = phy::qmp_status2_snapshot();
        SS_STATE_SNAPSHOT_QMP_POWER = phy::qmp_power_snapshot();
        let clocks = super::platform::bramble::usb_clock::read_usb_clock_register_state();
        SS_STATE_SNAPSHOT_QMP_BRANCHES = clocks.qmp_branches;
        let gdsc =
            core::ptr::read_volatile(super::platform::bramble::usb_resources().gdsc as *const u32);
        let snpsid = read(GSNPSID);
        let ltssm_reg = if matches!(snpsid >> 16, DWC31_IP | DWC32_IP) {
            DWC31_LINK_GDBGLTSSM
        } else {
            GDBGLTSSM
        };
        SS_STATE_SNAPSHOT_LTSSM = (read(ltssm_reg) >> 22) & 0xf;
        let domain = u32::from(known_dwc_core_ip(snpsid))
            | (u32::from(gdsc & (1 << 31) != 0) << 1)
            | (u32::from(
                clocks.controller_branches[0] & 1 != 0
                    && clocks.controller_branches[0] & (1 << 31) == 0,
            ) << 2)
            | (u32::from(
                clocks.controller_branches[3] & 1 != 0
                    && clocks.controller_branches[3] & (1 << 31) == 0,
            ) << 3);
        SS_STATE_SNAPSHOT_DOMAIN = domain;
        trace_event(
            TRACE_UTMI_STATE,
            0x0300_0000 | domain,
            snpsid,
            gdsc,
            clocks.core_source_config,
            clocks.utmi_source_config,
        );
        trace_event(
            TRACE_UTMI_STATE,
            0x0400_0000 | domain,
            clocks.controller_branches[0],
            clocks.controller_branches[3],
            clocks.controller_branches[1],
            clocks.controller_branches[2],
        );
        trace_event(
            TRACE_UTMI_STATE,
            0x0500_0000 | domain,
            clocks.controller_branches[4],
            clocks.controller_branches[5],
            0,
            0,
        );
        trace_event(
            TRACE_UTMI_STATE,
            0x0600_0000 | domain,
            clocks.qmp_branches[0],
            clocks.qmp_branches[1],
            clocks.qmp_branches[3],
            0,
        );
        trace_event(
            TRACE_UTMI_STATE,
            0x0700_0000 | domain,
            SS_STATE_SNAPSHOT_QSCRATCH_GENERAL,
            SS_STATE_SNAPSHOT_QSCRATCH,
            SS_STATE_SNAPSHOT_QMP_POWER,
            SS_STATE_SNAPSHOT_LTSSM,
        );
        SS_STATE_SNAPSHOT_VALID = true;
    }
}

/// Compute one of the live, read-only 4-bit USB2 controller words.
///
/// `domain` reuses the exact field extraction of `capture_ss_state_snapshot()`
/// (core IP answered, USB30 GDSC powered, core clock branch, mock-UTMI branch).
/// `blockers` encodes the three states the vendor DWC3 source names as stopping
/// device-event generation while the controller still retires endpoint
/// commands: DCTL.CSFTRST still asserted (gadget.c:2109-2119), GCTL.
/// CORESOFTRESET still asserted (dwc3-msm.c:2028-2030), and DEVTEN never
/// programmed (dwc3_gadget_enable_irq(), gadget.c:2324-2343). Unknown names
/// return 15 so a typo cannot masquerade as a valid zero word.
/// Capture the DWC3 core id into the crate-shared retained region, once.
///
/// Safe to call at any point where the controller answers: it no-ops after the first
/// successful read. Called from the handoff's power re-assert, which is the last moment the
/// aperture is known-good before the readout path runs.
pub fn latch_snpsid() {
    unsafe {
        if trace::SHARED_SNPSID == 0 {
            trace::SHARED_SNPSID = read(GSNPSID);
        }
    }
}

#[inline]
fn cached_snpsid() -> u32 {
    // Never touch the controller from here: measured to hang when the USB clock branch has
    // collapsed. A zero cache means the latch never ran, which is a *diagnostic* result, not
    // a reason to go reading MMIO. See usb/README.md §1.6.
    unsafe { trace::SHARED_SNPSID }
}

unsafe fn usb2_live_word(word: &str) -> u32 {
    // ---- Which GUSB2PHYCFG0 call site ran last? ----
    // Explicit tags, because `Location::caller()` recorded 0 through `#[inline]`
    // and every proxy predicate tried before this one answered a different
    // question. Ids: 1xxx = mod.rs, 2xxx = config.rs, 3xxx = control.rs.
    //
    //   g2wtag-eq-<id>  : the last writer was exactly this id
    //   g2wtag-ge-<id>  : the last writer's id is >= this (bisect within a file)
    //   g2wtag-file     : 0 = none yet, 1 = mod.rs, 2 = config.rs, 3 = control.rs
    if let Some(rest) = word.strip_prefix("g2wtag-eq-") {
        let id: u32 = rest.parse().unwrap_or(u32::MAX);
        return u32::from(G2W_SITE.load(core::sync::atomic::Ordering::Relaxed) == id);
    }
    if let Some(rest) = word.strip_prefix("g2wtag-ge-") {
        let id: u32 = rest.parse().unwrap_or(u32::MAX);
        let seen = G2W_SITE.load(core::sync::atomic::Ordering::Relaxed);
        return u32::from(seen != 0 && seen >= id);
    }
    if word == "g2wtag-file" {
        return G2W_SITE.load(core::sync::atomic::Ordering::Relaxed) / 1000;
    }
    // Route every `dwc3-*` word onto the proven CCS carrier.
    // See usb/README.md §3.9.
    if word.starts_with("dwc3-") {
        trace_dwc3_boundary();
        if word.starts_with("dwc3-debug-") || word.starts_with("dwc3-free-") {
            for _ in 0..6 {
                trace_dwc3_debug_sample();
                readout_keepalive_delay_ms(50);
            }
        }
        return trace::utmi_readout_code(word);
    }
    // "WENT" - the lookup body was entered. `usb2_live_word` runs TWICE per pass for a
    // `usb2-live-ccs-<word>` selector: once at :2275 via `utmi_readout_code`, once at :9441.
    // "WBFR" being present only proves the FIRST one finished. This marker separates entry
    // from the arm dispatch. See usb/README.md §3.17.
    trace_marker(TRACE_PROBE_WATCHDOG, 0x5745_4E54); // "WENT"
    unsafe {
        let snpsid = cached_snpsid();
        match word {
            "domain" => {
                let gdsc = core::ptr::read_volatile(
                    super::platform::bramble::usb_resources().gdsc as *const u32,
                );
                let clocks = super::platform::bramble::usb_clock::read_usb_clock_register_state();
                u32::from(known_dwc_core_ip(snpsid))
                    | (u32::from(gdsc & (1 << 31) != 0) << 1)
                    | (u32::from(
                        clocks.controller_branches[0] & 1 != 0
                            && clocks.controller_branches[0] & (1 << 31) == 0,
                    ) << 2)
                    | (u32::from(
                        clocks.controller_branches[3] & 1 != 0
                            && clocks.controller_branches[3] & (1 << 31) == 0,
                    ) << 3)
            }
            "blockers" => {
                u32::from(known_dwc_core_ip(snpsid))
                    | (u32::from(read(DCTL) & DCTL_CSFTRST != 0) << 1)
                    | (u32::from(read(GCTL) & GCTL_CORESOFTRESET != 0) << 2)
                    | (u32::from(read(DEVTEN) != 0) << 3)
            }
            "runstop" => {
                // Is DCTL.RUN_STOP actually asserted? The handoff's own
                // "RUN/STOP readback timed out" path has never been able to
                // publish this.
                u32::from(read(DCTL) & DCTL_RUN_STOP != 0)
            }
            "conspd" => {
                // What speed does the *controller* believe the link is at?
                // DSTS.CONNECTSPD is bits 2:0 (0=HS, 1=FS, 2=LS, 3=SS).
                read(DSTS) & 0x7
            }
            "evtcount" => {
                // Has anything landed in the EP0 event ring? Saturate at 3 so
                // the word stays readable on a 4-bit channel.
                (read(GEVNTCOUNT0) & GEVNTCOUNT_MASK).min(3)
            }
            // Shift the field down: the CCS channel carries only 4 bits, and a raw
            // mask leaves PRTCAPDIR (bits 13:12) at 0x2000 for DEVICE, which
            // truncates to zero pulses and reads exactly like "no capability
            // set at all". Measured wrong once; keep the shift.
            "prtcap" => (read(GCTL) & GCTL_PRTCAPDIR_MASK) >> 12,
            // Who wrote GCTL last? `mmio::write()` is `#[track_caller]`, so this
            // names the source line of the most recent GCTL write without that
            // site knowing anything about it. Three nibbles: `gctlw` = line
            // bits 3:0, `gctlw2` = line bits 7:4, `gctlw3` = the PRTCAPDIR field
            // that was written (0=OTG, 1=HOST, 2=DEVICE) plus bit3 = "source
            // line exceeded 8 bits". 0xf in any of the first two means the
            // packed value has not been stored (no GCTL write yet).
            "gctlw" => {
                let v = mmio::GCTL_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed);
                if v == u32::MAX { 0xf } else { v & 0xf }
            }
            "gctlw2" => {
                let v = mmio::GCTL_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed);
                if v == u32::MAX { 0xf } else { (v >> 4) & 0xf }
            }
            "gctlw3" => {
                let v = mmio::GCTL_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed);
                // NOTE: the store packs the field into bits 17:16, so the reader
                // must mask 0x3. A wider mask pulls in bit 18, which is bit 2 of
                // the line number - the two widths must match.
                if v == u32::MAX { 0xf } else { (v >> 16) & 0x3 }
            }
            // Full 8-bit low byte of the last GCTL writer's source line, in ONE
            // run. Deliberately a single word: splitting it across runs cannot
            // work, because each run is a separate boot with its own "last
            // writer" (see the history-probe note). `0xff` = no GCTL write yet.
            "gctlline" => {
                let v = mmio::GCTL_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed);
                if v == u32::MAX { 0xff } else { v & 0xff }
            }
            // Same value, shifted down by 8, for lines beyond 255.
            "gctlline2" => {
                let v = mmio::GCTL_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed);
                if v == u32::MAX { 0xff } else { (v >> 8) & 0xff }
            }
            // Four-bit halves of the writer line. The byte-wide `gctlline` emits
            // up to eight pulses, and each pulse is a host re-attach - which
            // breaks the "exactly one baseline attach" validity rule that all
            // the other selectors rely on (three consecutive 8-bit runs were
            // rejected on that rule). A nibble is at most four pulses, so the
            // existing rule applies unchanged. The two halves come from
            // different boots, so this is only sound if the write sequence is
            // deterministic - take each half twice and cross-check.
            // Sentinel: 0xf means "no GCTL write yet" in both halves.
            "gctllinelo" => {
                let v = mmio::GCTL_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed);
                if v == u32::MAX { 0xf } else { v & 0xf }
            }
            "gctllinehi" => {
                let v = mmio::GCTL_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed);
                if v == u32::MAX { 0xf } else { (v >> 4) & 0xf }
            }
            // Same-boot PRTCAPDIR transition test. One pulse per predicate, so
            // the pulse count is the count of true statements:
            //   bit0: the early sample (right after the DEVICE write) was DEVICE
            //   bit1: the current field value is DEVICE  (i.e. it did NOT revert)
            //   bit2: the two differ            (the hardware changed it on its own)
            //   bit3: the early sample was never taken
            "prtcapx" => {
                let early = config::EARLY_PRTCAPDIR.load(core::sync::atomic::Ordering::Relaxed);
                let now = (read(GCTL) & GCTL_PRTCAPDIR_MASK) >> 12;
                let early_dev = early == 2;
                let now_dev = now == 2;
                let differs = early != u32::MAX && early != now;
                u32::from(early_dev)
                    | (u32::from(now_dev) << 1)
                    | (u32::from(differs) << 2)
                    | (u32::from(early == u32::MAX) << 3)
            }
            // Single-predicate variants of `prtcapx`, one bit each, so a run
            // answers exactly one yes/no question (the house rule). `prtcapx`
            // packed four predicates and returned two true, which left three
            // possible combinations - a word that cannot be interpreted is not
            // a measurement.
            //   prtcapx0: the DEVICE write was reached and captured DEVICE
            //   prtcapx1: the field is DEVICE *now*
            //   prtcapx2: early and now differ (the hardware changed it)
            //   prtcapx3: the early sample was never taken
            // Phase-timing ladder: "the readout happens this many ms after the
            // DEVICE-mode write". One predicate per run, per the house rule -
            // `dtim16` = "delta < 16 ms", `dtim64` = "< 64 ms", `dtim256` =
            // "< 256 ms". A true reading is one pulse. Together they bracket how
            // long the handoff spends before the controller is running, inside
            // the host's ~588 ms descriptor-read window.
            // The other leg: how long after the *pull-up* (i.e. after the handset
            // presented itself, which is when the host starts its enumeration
            // attempt) does the DEVICE-mode write happen? One predicate per run:
            // `dtimr64` = "pull-up -> DEVICE write < 64 ms", `dtimr256` = "< 256 ms",
            // `dtimr1000` = "< 1000 ms".
            // Sanity predicate for the leg above. If the pull-up tick is *later*
            // than the DEVICE write tick - i.e. the latch fired on a pull-up that
            // happens after configuration - then `wrapping_sub` produces a huge
            // u32 and every `dtimr*` bound reads FALSE for the wrong reason.
            // "pullup before write" must be TRUE for any dtimr* reading to mean
            // anything.
            "dtimr_ok" => {
                let start = control::PULLUP_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                let early = config::EARLY_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                u32::from(
                    start != u32::MAX && early != u32::MAX && start <= early,
                )
            }
            // Same guard for the write->readout leg.
            "dtim_ok" => {
                let early = config::EARLY_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                let freq = crate::timer::frequency();
                if early == u32::MAX || freq == 0 {
                    0
                } else {
                    let now = (crate::timer::counter() / (freq / 1000) as u64) as u32;
                    u32::from(early <= now)
                }
            }
            // The leg the earlier attempt got wrong: handoff entry -> DEVICE write.
            // Guarded by `dtimh_ok` (entry <= write) so an underflow cannot be
            // mistaken for a long interval again.
            "dtimh64" | "dtimh256" | "dtimh1000" => {
                let start = HANDOFF_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                let early = config::EARLY_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                let limit = match word {
                    "dtimh64" => 64u32,
                    "dtimh256" => 256,
                    _ => 1000,
                };
                if start == u32::MAX || early == u32::MAX || start > early {
                    0
                } else {
                    u32::from(early.wrapping_sub(start) < limit)
                }
            }
            // Sign guard for the leg above: entry latch exists, write latch exists,
            // and entry <= write. Must be TRUE for any dtimh* reading to mean anything.
            // The real blocker candidate: `try_arm_setup` early-returns forever
            // unless `ENDPOINTS_READY` is set, and the ledger (written when it had
            // far fewer assignment sites) says it is only set in the Connect Done
            // branch - which never runs with GEVNTCOUNT0 == 0. There are now many
            // assignment sites including several in the deferred readout block, so
            // *measure* whether any of them ran rather than trusting the note.
            // One predicate per run, counted as bits:
            //   bit0 = ENDPOINTS_READY          bit1 = EP0_SETUP_ARMED
            //   bit2 = EP0_STATE == Setup       bit3 = DALEPENA bit0
            "endpoints" => {
                u32::from(ENDPOINTS_READY)
                    | (u32::from(EP0_SETUP_ARMED) << 1)
                    | (u32::from(EP0_STATE == Ep0State::Setup) << 2)
                    | (u32::from(read(DALEPENA) & 1 != 0) << 3)
            }
            // Branch-free probes: report what the selector string actually IS at
            // the moment the readout runs. `word == "armd"` did not fire even
            // though the code sits inside the right block and other words resolve
            // fine, so read the string itself instead of reasoning about a branch.
            //   word_len  = byte length of the suffix (saturating at 15)
            //   word_b0   = first byte & 0xf
            //   word_b1   = second byte & 0xf
            // For "armd": len 4, b0 = 'a' (0x61) & 0xf = 1, b1 = 'r' (0x72) & 0xf = 2.
            // NOTE: these take the suffix as an argument, so they must be evaluated
            // in the same scope `word` is bound in - hence they are plain matches
            // here rather than a helper that re-derives the selector.
            "word_len" => (word.len().min(15)) as u32,
            "word_b0" => u32::from(word.as_bytes().first().copied().unwrap_or(0) & 0xf),
            "word_b1" => u32::from(word.as_bytes().get(1).copied().unwrap_or(0) & 0xf),
            "armd" => DEFERRED_ARM_RESULT.load(core::sync::atomic::Ordering::Relaxed) & 1,
            "armd_res" => (DEFERRED_ARM_RESULT.load(core::sync::atomic::Ordering::Relaxed) >> 1) & 1,
            // Crate-independent probe progress, read from the retained trace.
            "probe_reach" => u32::from(prev_boot_probe_reach_code() >= 6),
            "probe_stage" => prev_boot_probe_reach_code(),
            "probe_s1" => u32::from(prev_boot_probe_reach_code() >= 2),
            "probe_s2" => u32::from(prev_boot_probe_reach_code() >= 3),
            "probe_s3" => u32::from(prev_boot_probe_reach_code() >= 4),
            "probe_s4" => u32::from(prev_boot_probe_reach_code() >= 5),
            "hop_any" => u32::from(prev_boot_probe_reach_code() >= 100),
            "hop_ge1" => u32::from(prev_boot_probe_reach_code() >= 101),
            "hop_ge2" => u32::from(prev_boot_probe_reach_code() >= 102),
            "hop_ge3" => u32::from(prev_boot_probe_reach_code() >= 103),
            "hop_ge4" => u32::from(prev_boot_probe_reach_code() >= 104),
            "hop_ge5" => u32::from(prev_boot_probe_reach_code() >= 105),
            "hop_ge6" => u32::from(prev_boot_probe_reach_code() >= 106),
            "hop_ge7" => u32::from(prev_boot_probe_reach_code() >= 107),
            "hop_ge8" => u32::from(prev_boot_probe_reach_code() >= 108),
            "hop_ge9" => u32::from(prev_boot_probe_reach_code() >= 109),
            "act_known" => u32::from(unsafe { LAST_CONTROL_ACTION } != 0),
            "act_datain" => u32::from(unsafe { LAST_CONTROL_ACTION } == 1),
            "act_statusin" => u32::from(unsafe { LAST_CONTROL_ACTION } == 2),
            "act_stall" => u32::from(unsafe { LAST_CONTROL_ACTION } == 7),
            "act_other" => {
                let a = unsafe { LAST_CONTROL_ACTION };
                u32::from(a >= 3 && a <= 6)
            }
            "ph_pending" => u32::from(unsafe { DATA_PHASE_PENDING_START }),
            "ph_len" => u32::from(unsafe { DATA_PHASE_PENDING_LEN } != 0),
            "ep0_data" => u32::from(EP0_STATE == Ep0State::Data),
            "ep0_status" => u32::from(EP0_STATE == Ep0State::Status),
            // "POL1" at the top of `poll()`. Distinguishes "the driver loop never ran"
            // from "it ran and still saw nothing" - crate-independent, same retained trace
            // as `probe_reach`. See usb/README.md §3.12.
            "poll_ran" => u32::from(prev_boot_poll_ran()),
            // 1 word, 1 predicate: "SIG" alone, no HOP fold. See usb/README.md §3.13.
            "sig_only" => u32::from(prev_boot_sig_only()),
            // Brackets the handoff call itself. See usb/README.md §3.14.
            "hd_entered" => u32::from(prev_boot_handoff_entered()),
            // Did a synchronous abort fire at all this boot? See usb/README.md §3.15.
            "exc_seen" => u32::from(prev_boot_sync_exception()),
            // UPSTREAM markers only. `selx` is kept: it is written at :9504, AFTER the whole
            // readout block, so a reader inside the block is upstream of it and the reading is
            // meaningful. "wbef"/"waft"/"wstall"/"went" were removed as tautologies.
            // See usb/README.md §3.17.
            "selx" => u32::from(prev_boot_marker_eq(0x5345_4C58)), // "SELX"
            "m15d" => u32::from(prev_boot_marker_eq(0x4D31_3544)), // "M15D", set at :7757+
            // CHAIN BISECT. These are written by the *condition expressions* of the POSTRUN
            // readout chain, which Rust evaluates in order, so each one marks a real position
            // in the chain. All four are upstream of the reader at :9489, which is why - unlike
            // every marker tried before - these are not tautologies. See usb/README.md §3.18.
            "md1" => u32::from(prev_boot_marker_eq(0x4D44_3141)), // branch at :8140
            "md2" => u32::from(prev_boot_marker_eq(0x4D44_3241)), // branch at :8611
            "md3" => u32::from(prev_boot_marker_eq(0x4D44_3341)), // branch at :9061
            "md4" => u32::from(prev_boot_marker_eq(0x4D44_3441)), // branch at :9431
            // Bisect the span between milestone 15 (:7772) and the pre-readout boundary. This was
            // the last un-instrumented region; the readout block itself was exonerated by running
            // with `--utmi-postrun-readout` omitted (host still times out at ~5.5 s). All four
            // writers are upstream of the reader at the readout, so these are real predicates.
            "ma1" => u32::from(prev_boot_marker_eq(0x4D41_3141)),
            "ma2" => u32::from(prev_boot_marker_eq(0x4D41_3241)),
            "ma3" => u32::from(prev_boot_marker_eq(0x4D41_3341)),
            "ma4" => u32::from(prev_boot_marker_eq(0x4D41_3441)),
            // PREVIOUS-BOOT far-side probes. Read from the retained slot, not the ring, so these
            // are the first words that can see past the readout block. See usb/README.md §3.20.
            "ph20" => u32::from(unsafe { trace::SHARED_POST_HANDOFF } >= 20),
            "ph21" => u32::from(unsafe { trace::SHARED_POST_HANDOFF } >= 21),
            "ph22" => u32::from(unsafe { trace::SHARED_POST_HANDOFF } >= 22),
            // Proves the slot survives a boot; written at the top of init_usb2_handoff().
            "ph99" => u32::from(unsafe { trace::SHARED_POST_HANDOFF } >= 99),
            "hd_returned" => u32::from(prev_boot_handoff_returned()),
            // PSTA ladder, one bit each, HOP-free. See usb/README.md §3.13.
            "psta1" => u32::from(prev_boot_psta_only(0x01)),
            "psta2" => u32::from(prev_boot_psta_only(0x02)),
            "psta12" => u32::from(prev_boot_psta_only(0x12)),
            "psta3" => u32::from(prev_boot_psta_only(0x03)),
            "psta4" => u32::from(prev_boot_psta_only(0x04)),
            "ep_ready" => u32::from(ENDPOINTS_READY),
            "ep_armed" => u32::from(EP0_SETUP_ARMED),
            "ep_state" => u32::from(EP0_STATE == Ep0State::Setup),
            "ep_dalep" => u32::from(read(DALEPENA) & 1 != 0),
            // Is the arm-blip A/B actually compiled into this image?
            //
            // The rundir reports `effective_build_child_environment:
            // FULLERENE_AARCH64_USB_ARM_BLIP=1`, yet `arm_blip_queue` produced
            // no host blip even with a 75 s hold (well past its 30 s fallback).
            // `arm_blip_queue` gates on `option_env!("FULLERENE_USB_ARM_BLIP")`,
            // a COMPILE-TIME constant, so the child environment reaching the
            // build script is not the same claim as that constant being
            // `Some("1")` in the compiled kernel. Publish the constant itself.
            "blipenv" => u32::from(option_env!("FULLERENE_USB_ARM_BLIP").is_some()),
            // Did a SETUP packet actually arrive? `EP0_SETUP_ARMED` is cleared
            // by the EP0 XferComplete handler when the armed transfer is
            // *consumed* (mod.rs:5468-5479), so its false value at readout is
            // the signature of a completed transfer, not a missing one. These
            // two words test the thing that actually matters: whether the host's
            // SETUP reached the device at all.
            "ep0seen" => u32::from(ep0_setup_packet_seen()),
            "setupdr" => u32::from(TRACE_HARVEST_SETUP > 0),
            // Which arm window is compiled in? `mod.rs:9177-9188` picks
            // 10_000 / 5_000 / 400 ms from cfg!()s, and the A/B that widened it
            // (`--usb2-long-setup-arm`) produced an identical host failure. That
            // only refutes the window if the window actually changed, so publish
            // the compiled-in selection itself rather than inferring it from
            // `effective_build_child_environment`, which records only the
            // harness process's env and never the `cargo_envs` this flag rides.
            "armwin_long" => u32::from(cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_long_setup_arm)),
            // Which SUSPHY A/B is compiled in? There are TWO env names and two
            // cfgs (build.rs:1100-1105):
            //   ..._USB2_SUSPHY        -> cfg(...usb2_susphy)         (--usb2-susphy)
            //   ..._USB2_SOURCE_SUSPHY -> cfg(...usb2_source_susphy)  (--usb2-source-susphy)
            // SUSPHY is SET in exactly two places (mod.rs:7116 behind
            // usb2_source_susphy, mod.rs:10522 inside init_with_super_speed)
            // and CLEARED in four (5301, 7665 A/Bs; 9418 behind
            // not(handoff_probe); 13450 unreachable unless the link reaches U0).
            // Measured: `susphy` reads 0 even WITH --usb2-source-susphy, so
            // publish which cfg is actually compiled in rather than assuming.
            "cfgsus" => u32::from(cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_source_susphy)),
            // The two halves of the SUSPHY contradiction, each one bit:
            //   atsusphy = line 7139 asserted GUSB2PHYCFG0.SUSPHY
            //   guardeng = send_ep_command_result actually cleared it for a command
            // ge-1 = TRUE says the milestone at 7359 was reached (after 7139) while
            // `susphy` reads 0; these two bits separate "the guard never engaged"
            // from "7139 never ran".
            // Value read back from GUSB2PHYCFG0 *immediately after* writing
            // SUSPHY - not at the post-run readout. If this is 0 while the
            // post-run `susphy` is also 0, the readback path is broken rather
            // than the bit being cleared later.
            "atsusphy" => u32::from(unsafe { SUSPHY_SET_IN_HANDOFF }),
            "rawbef" => u32::from((unsafe { SUSPHY_RAW_BEFORE } & GUSB2PHYCFG_SUSPHY) != 0),
            // ---- Who wrote GUSB2PHYCFG0 last? ----
            // `write()` is `#[track_caller]` and records the calling source line
            // plus the SUSPHY/ENBLSLPM bits of the written value for this
            // register, the same way GCTL_LAST_WRITER does for GCTL. One
            // instrumented function names every writer without touching any of
            // them, which is what the four refutations of the read-only
            // exclusion list were asking for.
            //
            //   g2wsusphy = the LAST write had SUSPHY set
            //   g2wenbl   = the LAST write had ENBLSLPM set
            //   g2wline_ge_<k> = that write's source line is >= k (bisect)
            //   g2wcount_ge_<k> = at least k writes to the register happened
            "g2wsusphy" => u32::from(unsafe { mmio::GUSB2PHYCFG_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed) } >> 16 & 1 != 0),
            // Counted behaviour, independent of the (broken) line readback:
            //   g2wset  = at least one write set SUSPHY
            //   g2wclr  = at least one write cleared SUSPHY
            // If set and clr are both 1 and susphy is 0, a clear provably ran
            // after the last set.
            // ---- A vs B: was SUSPHY present in the last READ? ----
            //   g2r_susphy  = the last read(GUSB2PHYCFG0) returned SUSPHY set
            //   g2rknown    = a read was recorded at all (not u32::MAX)
            // With g2wsusphy = FALSE:
            //   g2r_susphy = TRUE  -> an active clear (A)
            //   g2r_susphy = FALSE -> a read-modify-write of a value that was
            //                        already missing the bit (B)
            // The read that actually PRECEDED the last write. This is the
            // predicate the A/B question needs; `g2r_susphy` alone is only the
            // latest read and may postdate the write.
            //   g2rw_susphy = TRUE  + g2wsusphy = FALSE -> an active clear (A)
            //   g2rw_susphy = FALSE + g2wsusphy = FALSE -> a stale read was
            //                                             written back (B)
            "g2rw_susphy" => u32::from(
                (unsafe {
                    mmio::GUSB2PHYCFG_READ_BEFORE_LAST_WRITE.load(core::sync::atomic::Ordering::Relaxed)
                } & mmio::GUSB2PHYCFG_SUSPHY)
                    != 0,
            ),
            "g2rwknown" => u32::from(
                unsafe {
                    mmio::GUSB2PHYCFG_READ_BEFORE_LAST_WRITE.load(core::sync::atomic::Ordering::Relaxed)
                } != u32::MAX,
            ),
            "g2r_susphy" => u32::from(
                (unsafe { mmio::GUSB2PHYCFG_LAST_READ.load(core::sync::atomic::Ordering::Relaxed) }
                    & mmio::GUSB2PHYCFG_SUSPHY)
                    != 0,
            ),
            "g2rknown" => u32::from(
                unsafe { mmio::GUSB2PHYCFG_LAST_READ.load(core::sync::atomic::Ordering::Relaxed) }
                    != u32::MAX,
            ),
            "g2rge10" => u32::from(
                unsafe { mmio::GUSB2PHYCFG_READ_COUNT.load(core::sync::atomic::Ordering::Relaxed) } >= 10,
            ),
            "g2wset" => u32::from(unsafe { mmio::GUSB2PHYCFG_SET_COUNT.load(core::sync::atomic::Ordering::Relaxed) } >= 1),
            "g2wclr" => u32::from(unsafe { mmio::GUSB2PHYCFG_CLEAR_COUNT.load(core::sync::atomic::Ordering::Relaxed) } >= 1),
            // Saturation markers so "how many" is distinguishable from "at least
            // one" without a multi-bit readout.
            "g2wclr10" => u32::from(unsafe { mmio::GUSB2PHYCFG_CLEAR_COUNT.load(core::sync::atomic::Ordering::Relaxed) } >= 10),
            "g2wclr100" => u32::from(unsafe { mmio::GUSB2PHYCFG_CLEAR_COUNT.load(core::sync::atomic::Ordering::Relaxed) } >= 100),
            "g2wenbl" => u32::from(unsafe { mmio::GUSB2PHYCFG_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed) } >> 17 & 1 != 0),
            "rawaft" => u32::from((unsafe { SUSPHY_RAW_AFTER } & GUSB2PHYCFG_SUSPHY) != 0),
            // Low nibble of the raw readback, so a non-zero raw value that
            // happens to lack the SUSPHY bit is still distinguishable from an
            // all-zero readback. Multi-bit: pulse count is the popcount, so read
            // it as "0 = nothing came back" only.
            "rawbeflo" => (unsafe { SUSPHY_RAW_BEFORE } & 0xf),
            "rawaftlo" => (unsafe { SUSPHY_RAW_AFTER } & 0xf),
            "guardeng" => u32::from(unsafe { CMD_GUARD_ENGAGED }),
            "cfgsus2" => u32::from(cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_susphy)),
            "armwin_ext" => u32::from(cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_extended_setup_arm)),
            // Did `arm_blip_queue` ever reach the point after the gate?
            "blipq" => u32::from(ARM_BLIP_QUEUED),
            // Was a blip ever emitted?
            "blipdone" => u32::from(ARM_BLIP_DONE),
            "dtimh_ok" => {
                let start = HANDOFF_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                let early = config::EARLY_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                u32::from(start != u32::MAX && early != u32::MAX && start <= early)
            }
            "dtimr64" | "dtimr256" | "dtimr1000" => {
                let start = control::PULLUP_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                let early = config::EARLY_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                let limit = match word {
                    "dtimr64" => 64u32,
                    "dtimr256" => 256,
                    _ => 1000,
                };
                if start == u32::MAX || early == u32::MAX {
                    0
                } else {
                    u32::from(early.wrapping_sub(start) < limit)
                }
            }
            "dtim16" | "dtim64" | "dtim256" => {
                let early = config::EARLY_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                let now = if crate::timer::frequency() == 0 {
                    u32::MAX
                } else {
                    (crate::timer::counter() / (crate::timer::frequency() / 1000) as u64) as u32
                };
                let limit = match word {
                    "dtim16" => 16u32,
                    "dtim64" => 64,
                    _ => 256,
                };
                if early == u32::MAX || now == u32::MAX {
                    0
                } else {
                    u32::from(now.wrapping_sub(early) < limit)
                }
            }
            "prtcapx0" => {
                let e = config::EARLY_PRTCAPDIR.load(core::sync::atomic::Ordering::Relaxed);
                u32::from(e == 2)
            }
            "prtcapx1" => u32::from(((read(GCTL) & GCTL_PRTCAPDIR_MASK) >> 12) == 2),
            "prtcapx2" => {
                let e = config::EARLY_PRTCAPDIR.load(core::sync::atomic::Ordering::Relaxed);
                let n = (read(GCTL) & GCTL_PRTCAPDIR_MASK) >> 12;
                u32::from(e != u32::MAX && e != n)
            }
            "prtcapx3" => {
                let e = config::EARLY_PRTCAPDIR.load(core::sync::atomic::Ordering::Relaxed);
                u32::from(e == u32::MAX)
            }
            // Writer identity AND the write count in ONE four-bit word, so a
            // single valid run is self-contained.
            //
            // The nibble split above assumed the last GCTL writer's line is the
            // same on every boot, and it is not: `gctllinelo` read 0x1 while
            // `gctllinehi` read 0x0, and no write site has low byte 0x01. Both
            // runs were valid, so both readings are real - the line varies.
            // Combining halves from different boots is therefore unsound, and
            // this word exists to stop doing it.
            //
            // Layout: bits 1:0 = (line & 0xf), bits 3:2 = the writer count saturating
            // at 3. Read it as: how many GCTL writes happened, and what does the
            // most recent one's line look like, *in the same boot*.
            "gctlwho" => {
                let v = mmio::GCTL_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed);
                let n = mmio::GCTL_WRITE_COUNT.load(core::sync::atomic::Ordering::Relaxed);
                let lo = if v == u32::MAX { 0xf } else { v & 0xf };
                (lo & 0x3) | ((n.min(3) & 0x3) << 2)
            }
            // Saturating GCTL write counter, low nibble.
            // Byte-wide so a count above 15 is distinguishable, and so the same
            // readout path as `gctlline` is exercised (a control for whether the
            // byte-wide branch is reached at all).
            "gctlwc" => mmio::GCTL_WRITE_COUNT
                .load(core::sync::atomic::Ordering::Relaxed)
                .min(0xff),
            // Proof-of-execution for the DEVICE-mode write, published in two
            // words so 4 bits are enough: `devmode` = (landed, wrote) and
            // `devmode2` = the field value read back at the write site.
            // `devmode`: bit0 = the readback matched what was written,
            // bits 3:1 = the value written (2 = DEVICE).
            "devmode" => {
                let probe = DEVICE_MODE_WRITE_PROBE.load(core::sync::atomic::Ordering::Relaxed);
                if probe == u32::MAX {
                    0xf // never executed
                } else {
                    (probe & 1) | (((probe >> 8) & 0x7) << 1)
                }
            }
            // `devmode2`: bits 3:0 = the field value read back immediately.
            "devmode2" => {
                let probe = DEVICE_MODE_WRITE_PROBE.load(core::sync::atomic::Ordering::Relaxed);
                if probe == u32::MAX {
                    0xf
                } else {
                    (probe >> 16) & 0xf
                }
            }
            "dctlrs" => u32::from(read(DCTL) & DCTL_RUN_STOP != 0),
            "gctl_lo" => read(GCTL) & 0xf,
            "runstop_and_speed" => {
                // Pack both halves of the chirp question into one word:
                // bit0 = RUN_STOP asserted, bits 2:1 = CONNECTSPD (0=HS,1=FS,2=LS).
                u32::from(read(DCTL) & DCTL_RUN_STOP != 0) | ((read(DSTS) & 0x3) << 1)
            }
            "utmic0lo" => unsafe { phy::utmi_ctrl0() & 0xf },
            "utmic0hi" => unsafe { (phy::utmi_ctrl0() >> 4) & 0xf },
            "cfg0lo" => unsafe { phy::cfg0() & 0xf },
            "cfg0hi" => unsafe { (phy::cfg0() >> 4) & 0xf },
            "suspn" => u32::from(unsafe { phy::suspend_n_asserted() }),
            "common0hi" => unsafe { (read_volatile(hsphy_reg(HSPHY_COMMON0)) >> 4) & 0xf },
            "cfg0hi" => unsafe { (phy::cfg0() >> 4) & 0xf },
            "gphycfg_lo" => unsafe { read(GUSB2PHYCFG0) & 0xf },
            "gphycfg_hi" => unsafe { (read(GUSB2PHYCFG0) >> 4) & 0xf },
            "lnkst" => {
                // DSTS.USBLNKST - what link state does the controller think it
                // is in? The phy.rs comment predicts a bogus "On" is possible.
                (read(DSTS) >> 22) & 0xf
            }
            "susphy" => {
                // GUSB2PHYCFG.SUSPHY - is the PHY being told to suspend, which
                // would stop the parallel receive path while USB2 PHY clocks
                // keep running?
                u32::from(read(GUSB2PHYCFG0) & GUSB2PHYCFG_SUSPHY != 0)
            }
            "halted" => {
                // DSTS.DEVCTRLHLT - did the controller leave the halted state?
                u32::from(read(DSTS) & DSTS_DEVCTRLHLT != 0)
            }
            _ => 15,
        }
    }
}

pub fn utmi_readout_code(selector: &str) -> u32 {
    if selector == "protocol" {
        return protocol_readout_code();
    }
    if selector == "dwc3-hib" {
        // Read-only source discriminator for qpr1's
        // dwc3_gadget_run_stop(true) KEEP_CONNECT branch. The low-power
        // option is a hardware capability field, not a software state bit;
        // publish only the categorical supported/not-supported result.
        unsafe {
            return u32::from(
                read(GHWPARAMS1) & GHWPARAMS1_EN_PWROPT_MASK == GHWPARAMS1_EN_PWROPT_HIB,
            );
        }
    }
    if selector == "hsphy-table" {
        // The PHY tuning table source is decided at DTB-install time in
        // main.rs, before any DWC3/PHY access. Bit 0-1: 0 = compiled
        // fallback, 1 = two-entry DT override, 2 = three-entry DT override.
        // Bit 8: the QMP table also came from the DT. The direct USB2 path
        // never touches the QMP poll, so the low bits are the relevant
        // classification there.
        return hsphy_table_source();
    }
    if let Some(cell) = selector.strip_prefix("hsphy-prop-") {
        // Categorical DT-observation codes (see hsphy_prop_code): tiny
        // values that survive the .min(15) clamp of the attach-delay
        // ladder. Aspects: present (0/1), len (0=absent, 1/2/3 = 8/16/24
        // bytes, 4 = other), pair0/1/2 (0=absent/incomplete, 1 = exact
        // qpr1 base value, 2 = known alternate, 3 = other).
        return phy_tables::hsphy_prop_code(cell);
    }
    if let Some(aspect) = selector.strip_prefix("hsphy-node-") {
        // Identity codes are categorical; never send a raw 0x088e3000 value
        // through the four-bit attach-delay channel.
        return phy_tables::hsphy_node_code(aspect);
    }
    if let Some(rest) = selector.strip_prefix("usb2-live-") {
        // Live, read-only 4-bit controller words for the USB2-only handoff.
        // The `ss-domain-*` selectors replay the SuperSpeed snapshot, which a
        // direct USB2 run never captures; these words are sampled at the
        // readout site itself. An optional transport prefix selects the
        // post-Run/Stop variant: `park-` publishes through the probe's own
        // PSCI reset time, `blip-` through a fresh stop/run pair; the bare
        // form is the pre-connect (attach-delay) transport.
        let word = rest
            .strip_prefix("park-")
            .or_else(|| rest.strip_prefix("blip-"))
            .unwrap_or(rest);
        // The `ccs-` transport prefix is part of the selector, not the word name.
        // Without this strip, `usb2-live-ccs-armd` arrives as `ccs-armd`, matches
        // no arm in `usb2_live_word`'s match, and falls through to the default -
        // which is why every word added here silently published a constant while
        // the readout block at `mod.rs:9104` (which *does* strip "usb2-live-ccs-")
        // was never reached. Measured: `word_len` published 1, not 8.
        let word = word.strip_prefix("ccs-").unwrap_or(word);
        // A/B: arm EP0 synchronously here, in the scope that actually executes.
        //
        // An earlier attempt put this gate in the readout block's
        // `strip_prefix("usb2-live-ccs-")` scope, which is *dead* - `usb2-live-`
        // above matches first, so that branch (and its `usb2_live_word(word)`
        // call at ~9104) is never reached. Measured: the gate never fired and the
        // log marker stayed at zero. This is the live scope; the same `word` that
        // `usb2_live_word` receives is the one tested here.
        //
        // Rationale for the arm itself: on valid plain-profile runs
        // `ENDPOINTS_READY` is true and `EP0_STATE == Setup`, yet
        // `EP0_SETUP_ARMED` is false - every `try_arm_setup` guard is false, so the
        // function would proceed if called, and none of its sixteen call sites is
        // in the handoff path. Arming here removes the dependence on
        // poll()/timer-IRQ timing beating the host's first SETUP token.
        if word == "armd" {
            let armed = unsafe { try_arm_setup() };
            DEFERRED_ARM_RESULT.store(
                1 | (u32::from(armed) << 1),
                core::sync::atomic::Ordering::Relaxed,
            );
            log_hex("usb deferred: arm attempted=", u64::from(armed));
        }
        // Read the arm result at the one point this profile is known to reach.
        //
        // `arm_blip_queue` is called only from `main.rs:930/950` (the normal
        // handoff entry) and `usb_probe.rs:2826` (behind `gadget_ready`). The
        // `--direct-handoff` profile reaches neither, which is why `blipq`
        // published FALSE while `blipenv` published TRUE - the A/B is compiled
        // in but never queued. Call it here, gated on the selector so ordinary
        // readouts are untouched:
        //
        //   --utmi-postrun-readout usb2-live-ccs-blipq    -> queue reached?
        //   --utmi-postrun-readout usb2-live-ccs-blipdone -> blip emitted?
        //
        // `arm_blip_queue` ends in `runstop_blips`, which toggles Run/Stop, and
        // handoff-path Run/Stop is measured destructive to the attach. Read
        // `blipq`/`blipdone` together with the attach count, not the attach
        // count alone.
        if word == "blipq" || word == "blipdone" {
            unsafe { arm_blip_queue() };
        }
        return unsafe { usb2_live_word(word) };
    }
    if selector.starts_with("ss-") {
        // A failed handoff can enter the generic signal path and publish the
        // same USB2 marker without ever reaching stage 13/21. Reserve 15 for
        // that case so a missing SS snapshot cannot masquerade as a valid
        // zero-valued register field.
        unsafe {
            if !SS_STATE_SNAPSHOT_VALID {
                return if selector == "ss-domain" {
                    31
                } else if selector.starts_with("ss-domain-") {
                    2
                } else if selector.starts_with("ss-gctl-") {
                    2
                } else if selector.starts_with("ss-qmp-") {
                    2
                } else if selector.starts_with("ss-qscratch-") {
                    2
                } else if selector == "ss-ltssm" {
                    16
                } else if selector.starts_with("ss-ltssm-bit") {
                    2
                } else {
                    15
                };
            }
        }
    }
    if matches!(
        selector,
        "ss-speed"
            | "ss-link"
            | "ss-pipe"
            | "ss-gctl"
            | "ss-qmp"
            | "ss-vbus"
            | "ss-dctl"
            | "ss-domain"
            | "ss-domain-core"
            | "ss-domain-gdsc"
            | "ss-domain-core-branch"
            | "ss-domain-utmi-branch"
            | "ss-gctl-device"
            | "ss-qmp-phystatus"
            | "ss-qmp-start0"
            | "ss-qmp-start1"
            | "ss-qmp-typec0"
            | "ss-qmp-rxeq"
            | "ss-qmp-com-power"
            | "ss-qmp-pcs-power"
            | "ss-qmp-aux-branch"
            | "ss-qmp-pipe-branch"
            | "ss-qmp-com-aux-branch"
            | "ss-qscratch-utmi-sel"
            | "ss-qscratch-phystatus-sw"
            | "ss-qscratch-utmi-dis"
            | "ss-ltssm"
            | "ss-ltssm-bit0"
            | "ss-ltssm-bit1"
            | "ss-ltssm-bit1-wide"
            | "ss-ltssm-bit2"
            | "ss-ltssm-bit3"
    ) {
        unsafe {
            let dsts = if SS_STATE_SNAPSHOT_VALID {
                SS_STATE_SNAPSHOT_DSTS
            } else {
                read(DSTS)
            };
            let dctl = if SS_STATE_SNAPSHOT_VALID {
                SS_STATE_SNAPSHOT_DCTL
            } else {
                read(DCTL)
            };
            let pipe = if SS_STATE_SNAPSHOT_VALID {
                SS_STATE_SNAPSHOT_PIPE
            } else {
                read(GUSB3PIPECTL0)
            };
            let gctl = if SS_STATE_SNAPSHOT_VALID {
                SS_STATE_SNAPSHOT_GCTL
            } else {
                read(GCTL)
            };
            let qscratch = if SS_STATE_SNAPSHOT_VALID {
                SS_STATE_SNAPSHOT_QSCRATCH
            } else {
                read_qscratch(QSCRATCH_SS_PHY_CTRL)
            };
            let qscratch_general = if SS_STATE_SNAPSHOT_VALID {
                SS_STATE_SNAPSHOT_QSCRATCH_GENERAL
            } else {
                read_qscratch(QSCRATCH_GENERAL_CFG)
            };
            let qmp = if SS_STATE_SNAPSHOT_VALID {
                SS_STATE_SNAPSHOT_QMP
            } else {
                phy::qmp_post_runstop_snapshot()
            };
            let ltssm = if SS_STATE_SNAPSHOT_VALID {
                SS_STATE_SNAPSHOT_LTSSM
            } else {
                gdb_ltssm_link_state()
            };
            return match selector {
                // DWC3 DSTS.CONNECTSPD: 4 = SuperSpeed, 5 = SuperSpeed+.
                "ss-speed" => dsts & DSTS_CONNECTSPD_MASK,
                // DWC3 DSTS.USBLNKST: U0=0, RX_DET=5, SS_INACT=6,
                // POLL=7, and the remaining states follow the DWC3 table.
                "ss-link" => (dsts >> 18) & 0xf,
                // Four independent bits: USB3 PIPE SUSPHY, PIPE soft reset,
                // DCTL Run/Stop, and DSTS halted.
                "ss-pipe" => {
                    (u32::from(pipe & GUSB3PIPECTL_SUSPHY != 0))
                        | (u32::from(pipe & GUSB3PIPECTL_PHYSOFTRST != 0) << 1)
                        | (u32::from(dctl & DCTL_RUN_STOP != 0) << 2)
                        | (u32::from(dsts & DSTS_DEVCTRLHLT != 0) << 3)
                }
                // GCTL.PRTCAPDIR plus the two-bit RAMCLKSEL value.
                "ss-gctl" => ((gctl & GCTL_PRTCAPDIR_MASK) >> 12) | (gctl_ramclksel(gctl) << 2),
                // QMP PHYSTATUS, PCS_START_CONTROL[1:0], and TYPEC_CTRL[0].
                "ss-qmp" => {
                    u32::from(qmp & QMP_PHYSTATUS != 0)
                        | (((qmp >> 16) & 0x3) << 1)
                        | (((qmp >> 24) & 0x1) << 3)
                }
                "ss-vbus" => (qscratch >> 24) & 0x1,
                // Run/Stop lifecycle: pre-write DCTL, immediate post-write
                // DCTL, later stage-21 DCTL, and later DSTS halted.
                "ss-dctl" => {
                    let pre = SS_RUNSTOP_PRE_DCTL;
                    let post = SS_RUNSTOP_POST_DCTL;
                    let post_dsts = SS_RUNSTOP_POST_DSTS;
                    u32::from(pre != 0xffff_ffff && pre & DCTL_RUN_STOP != 0)
                        | (u32::from(post != 0xffff_ffff && post & DCTL_RUN_STOP != 0) << 1)
                        | (u32::from(dctl & DCTL_RUN_STOP != 0) << 2)
                        | (u32::from(post_dsts != 0xffff_ffff && post_dsts & DSTS_DEVCTRLHLT != 0)
                            << 3)
                }
                // Encode the four domain bits as N+1 so 1..=16 are valid
                // snapshots and 31 remains the missing-snapshot sentinel.
                "ss-domain" => SS_STATE_SNAPSHOT_DOMAIN + 1,
                "ss-domain-core" => SS_STATE_SNAPSHOT_DOMAIN & 1,
                "ss-domain-gdsc" => (SS_STATE_SNAPSHOT_DOMAIN >> 1) & 1,
                "ss-domain-core-branch" => (SS_STATE_SNAPSHOT_DOMAIN >> 2) & 1,
                "ss-domain-utmi-branch" => (SS_STATE_SNAPSHOT_DOMAIN >> 3) & 1,
                // DWC3 GCTL.PRTCAPDIR: 2 = DEVICE. This is separated from
                // the clock/GDSC domain bits because a live MMIO readback
                // does not prove that the controller retained its protocol
                // capability through the no-core handoff.
                "ss-gctl-device" => u32::from(gctl & GCTL_PRTCAPDIR_MASK == GCTL_PRTCAP_DEVICE),
                "ss-qmp-phystatus" => u32::from(qmp & QMP_PHYSTATUS != 0),
                "ss-qmp-start0" => (qmp >> 16) & 1,
                "ss-qmp-start1" => (qmp >> 17) & 1,
                "ss-qmp-typec0" => (qmp >> 24) & 1,
                // QMP PCS_STATUS2[3] is the official Qualcomm
                // RX_EQUALIZATION_IN_PROGRESS indicator.
                "ss-qmp-rxeq" => u32::from(SS_STATE_SNAPSHOT_QMP_STATUS2 & (1 << 3) != 0),
                "ss-qmp-com-power" => u32::from(SS_STATE_SNAPSHOT_QMP_POWER & 1 != 0),
                "ss-qmp-pcs-power" => u32::from(SS_STATE_SNAPSHOT_QMP_POWER & 2 != 0),
                "ss-qmp-aux-branch" => u32::from(
                    SS_STATE_SNAPSHOT_QMP_BRANCHES[0] & 1 != 0
                        && SS_STATE_SNAPSHOT_QMP_BRANCHES[0] & (1 << 31) == 0,
                ),
                "ss-qmp-pipe-branch" => u32::from(
                    SS_STATE_SNAPSHOT_QMP_BRANCHES[1] & 1 != 0
                        && SS_STATE_SNAPSHOT_QMP_BRANCHES[1] & (1 << 31) == 0,
                ),
                "ss-qmp-com-aux-branch" => u32::from(
                    SS_STATE_SNAPSHOT_QMP_BRANCHES[3] & 1 != 0
                        && SS_STATE_SNAPSHOT_QMP_BRANCHES[3] & (1 << 31) == 0,
                ),
                // Qualcomm's SS path leaves the UTMI-as-PIPE mux sequence
                // alone; the glue only applies it for HS/full-speed. These
                // are read-only bits from the selected same-boot snapshot so
                // a stale Fastboot HS mux can be separated from QMP state.
                "ss-qscratch-utmi-sel" => u32::from(qscratch_general & (1 << 0) != 0),
                "ss-qscratch-phystatus-sw" => u32::from(qscratch_general & (1 << 3) != 0),
                "ss-qscratch-utmi-dis" => u32::from(qscratch_general & (1 << 8) != 0),
                // Qualcomm msm's DWC3 glue reads LINKSTATE from bits 25:22
                // of the USB31 link-debug register. This is a raw four-bit
                // snapshot from the selected boundary, separate from
                // DSTS.USBLNKST. Stage 20 is pre-Run/Stop and normally reads
                // SS_DIS; use stage 21 to classify the running link.
                "ss-ltssm" => ltssm,
                "ss-ltssm-bit0" => ltssm & 1,
                "ss-ltssm-bit1" => (ltssm >> 1) & 1,
                "ss-ltssm-bit1-wide" => (ltssm >> 1) & 1,
                "ss-ltssm-bit2" => (ltssm >> 2) & 1,
                "ss-ltssm-bit3" => (ltssm >> 3) & 1,
                _ => 0,
            };
        }
    }
    trace::utmi_readout_code(selector)
}

/// Capture the controller boundary immediately before a protocol readout.
/// This is read-only: it does not acknowledge the event ring, alter the
/// endpoint command registers, or touch the USB2 PHY. Three records preserve
/// enough raw state to distinguish an empty event FIFO, a received SETUP, an
/// unconsumed SETUP TRB, and a stuck EP0 command after the host reports -71.
pub fn trace_dwc3_boundary() {
    unsafe {
        let event_count = read(GEVNTCOUNT0);
        let dsts = read(DSTS);
        let dctl = read(DCTL);
        let devten = read(DEVTEN);
        let dalepena = read(DALEPENA);
        let depcmd0 = read(dep_reg(0, 0x0c));
        let depcmd1 = read(dep_reg(1, 0x0c));
        let trb0 = ep0_trb_ptr(0);
        let trb1 = ep0_trb_ptr(ep0_trb_index(1));
        cache_invalidate(trb0 as usize, core::mem::size_of::<Trb>());
        cache_invalidate(trb1 as usize, core::mem::size_of::<Trb>());
        let trb0_ctrl = read_volatile(addr_of!((*trb0).ctrl));
        let trb1_ctrl = read_volatile(addr_of!((*trb1).ctrl));
        let setup = ep0_setup_data_ptr() as *const u8;
        cache_invalidate(setup as usize, 8);
        let setup0 = u32::from_le_bytes([
            read_volatile(setup),
            read_volatile(setup.add(1)),
            read_volatile(setup.add(2)),
            read_volatile(setup.add(3)),
        ]);
        let setup1 = u32::from_le_bytes([
            read_volatile(setup.add(4)),
            read_volatile(setup.add(5)),
            read_volatile(setup.add(6)),
            read_volatile(setup.add(7)),
        ]);
        let setup_nonzero = (setup0 | setup1) != 0;
        let compact = u32::from((event_count & GEVNTCOUNT_MASK) != 0)
            | (u32::from(setup_nonzero) << 1)
            | (u32::from((trb0_ctrl & TRB_HWO) != 0) << 2)
            | (u32::from(SIGNAL_DWC3_DEVICE_ERROR) << 3);
        trace_event(
            TRACE_DWC3_BOUNDARY,
            0x4457_4300,
            event_count,
            dsts,
            dctl,
            devten,
        );
        trace_event(
            TRACE_DWC3_BOUNDARY,
            0x4457_4301,
            dalepena,
            depcmd0,
            depcmd1,
            compact,
        );
        trace_event(
            TRACE_DWC3_BOUNDARY,
            0x4457_4302,
            trb0_ctrl,
            trb1_ctrl,
            setup0,
            setup1,
        );
    }
}

/// Sample Synopsys internal debug queues and device-mode LSP values without
/// consuming events or changing endpoint state. This mirrors Linux's
/// `dwc3_core_fifo_space()` and gadget `lsp_dump` read paths.
pub fn trace_dwc3_debug_sample() {
    unsafe {
        let mut values = [0u32; 8];
        for queue_type in 0..8u32 {
            write(
                GDBGFIFOSPACE,
                (queue_type << GDBGFIFOSPACE_TYPE_SHIFT) & GDBGFIFOSPACE_TYPE_MASK,
            );
            values[queue_type as usize] = read(GDBGFIFOSPACE) >> GDBGFIFOSPACE_SPACE_SHIFT;
        }
        trace::live_dwc3_debug_sample(values, [0; 16], read(GDBGEPINFO0), read(GDBGEPINFO1));
        trace_event(
            TRACE_DWC3_DEBUG,
            0,
            values[0],
            values[1],
            values[2],
            values[3],
        );
        trace_event(
            TRACE_DWC3_DEBUG,
            1,
            values[4],
            values[5],
            values[6],
            values[7],
        );
    }
}

pub fn trace_dwc3_debug_window_sample(queue: usize) {
    unsafe {
        if queue >= DWC3_DEBUG_QUEUE_COUNT {
            return;
        }
        write(
            GDBGFIFOSPACE,
            ((queue as u32) << GDBGFIFOSPACE_TYPE_SHIFT) & GDBGFIFOSPACE_TYPE_MASK,
        );
        let value = read(GDBGFIFOSPACE) >> GDBGFIFOSPACE_SPACE_SHIFT;
        trace::live_dwc3_debug_window_sample(queue, value);
    }
}

/// Read the DWC3 gadget debug LSP vector without touching the transfer path.
/// Linux's DWC3 debugfs gadget-LSP reader selects device endpoints 0..15
/// through GDBGLSPMUX and then reads GDBGLSP. Keep this operation at bounded
/// handoff stages; the observation loop must not continuously rewrite the
/// debug mux while a host transaction is in flight.
unsafe fn read_dwc3_gadget_lsp() -> [u32; 16] {
    let mut values = [0u32; 16];
    for endpoint in 0..16u32 {
        write(
            GDBGLSPMUX,
            (endpoint << GDBGLSPMUX_DEVSELECT_SHIFT) & 0x0000_00f0,
        );
        values[endpoint as usize] = read(GDBGLSP);
    }
    values
}

/// Return the one queue selected by a descriptor-window readout. Entry/stage
/// selectors deliberately return None so the observation loop performs no
/// continuous GDBGFIFOSPACE or GDBGLSPMUX writes after the stage latch.
pub fn dwc3_debug_window_queue() -> Option<usize> {
    match option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") {
        Some("dwc3-free-descriptor-window-txfifo") => Some(DWC3_DEBUG_QUEUE_TXFIFO),
        Some("dwc3-free-descriptor-window-rxfifo") => Some(DWC3_DEBUG_QUEUE_RXFIFO),
        Some("dwc3-free-descriptor-window-txreq") => Some(DWC3_DEBUG_QUEUE_TXREQQ),
        Some("dwc3-free-descriptor-window-rxreq") => Some(DWC3_DEBUG_QUEUE_RXREQQ),
        Some("dwc3-free-descriptor-window-rxinfo") => Some(DWC3_DEBUG_QUEUE_RXINFOQ),
        Some("dwc3-free-descriptor-window-pstat") => Some(DWC3_DEBUG_QUEUE_PSTATQ),
        Some("dwc3-free-descriptor-window-descfetch") => Some(DWC3_DEBUG_QUEUE_DESCFETCHQ),
        Some("dwc3-free-descriptor-window-eventq") => Some(DWC3_DEBUG_QUEUE_EVENTQ),
        _ => None,
    }
}

/// Capture only the DWC3 queue SPACE_AVAILABLE vector at a named handoff stage.
/// The value is free space, not occupancy.
pub fn trace_dwc3_debug_stage(stage: u32) {
    unsafe {
        let mut values = [0u32; DWC3_DEBUG_QUEUE_COUNT];
        for queue_type in 0..DWC3_DEBUG_QUEUE_COUNT as u32 {
            write(
                GDBGFIFOSPACE,
                (queue_type << GDBGFIFOSPACE_TYPE_SHIFT) & GDBGFIFOSPACE_TYPE_MASK,
            );
            values[queue_type as usize] = read(GDBGFIFOSPACE) >> GDBGFIFOSPACE_SPACE_SHIFT;
        }
        let lsp = read_dwc3_gadget_lsp();
        trace::live_dwc3_debug_sample(values, lsp, read(GDBGEPINFO0), read(GDBGEPINFO1));
        trace::live_dwc3_debug_stage(stage, values);
        trace_event(
            TRACE_DWC3_DEBUG,
            0x100 | (stage & 0xff),
            values[DWC3_DEBUG_QUEUE_TXFIFO],
            values[DWC3_DEBUG_QUEUE_RXFIFO],
            values[DWC3_DEBUG_QUEUE_RXREQQ],
            values[DWC3_DEBUG_QUEUE_EVENTQ],
        );
    }
}

/// Classify the first retained EP0 STARTTRANSFER boundary for a host-visible
/// protocol-error readout. This is deliberately a command/ownership result,
/// not a claim about CRC or bit stuffing on the USB wires:
///   0 = no EP0 STARTTRANSFER record
///   1 = first EP0 STARTTRANSFER timed out with CMDACT still set
///   2 = first command completed with DWC3 status 0x1 (No Resource)
///   3 = first command completed with another non-zero DWC3 status
///   4 = first command completed with status 0
///   5 = software later received at least one SETUP packet
/// The signal probe maps this code to code+1 same-boot DCTL stop/run attach
/// cycles, so the result remains observable even when the current image never
/// reaches an enumerated trace transport.
pub fn protocol_readout_code() -> u32 {
    unsafe {
        harvest_trace_outcome();
        if TRACE_HARVEST_SETUP > 0 {
            return 5;
        }
        if TRACE_HARVEST == 0xFFFF_FFFF {
            return 0;
        }
        if TRACE_HARVEST & 0x8000_0000 != 0 {
            return 1;
        }
        match TRACE_HARVEST & 0xf000 {
            0 => 4,
            0x1000 => 2,
            _ => 3,
        }
    }
}

/// True when the newest EP1 transfer-complete event reports success (status
/// 0): the read/64 data phase left the core and the host should hold the
/// descriptor bytes. The mrad tail stable-parks on this instead of resetting,
/// because a live data phase is worth keeping the session up for.
pub fn ep1_data_phase_complete() -> bool {
    unsafe {
        harvest_trace_outcome();
        TRACE_HARVEST_EP1_XFER == 0
    }
}

/// Eval-time rescue for the read/64 -110. No blip readout has ever been
/// host-visible on this board (zero SDIS pairs across every run), so the
/// diag gate ACTS instead of reporting: it re-drives whichever stage of the
/// host's pending 64-byte GET_DESCRIPTOR is stuck, using the live trace
/// plus the live core state. The host's read/64 URB stays pending until its
/// 5 s timeout and keeps polling IN tokens, so a successful re-arm lands
/// the data and the host journal's enumeration progress is the readout
/// (1234:0001 = success, -110 again = this stage's rescue did not land).
/// Returns the branch taken (0 = no rescue):
///   0 = last SETUP was not a 64-byte GET_DESCRIPTOR, or the core is not
///       running with the link U0 (nothing pending to rescue)
///   1 = latched SETUP undelivered (the setup buffer still holds the
///       packet): re-dispatched through handle_setup
///   2 = nothing latched, state Setup: the SETUP TRB was never (re)armed,
///       so the host's SETUP is latched in the core - rearm it
///   3 = state Data: the 64-byte DataIn was dispatched but the data phase
///       never completed - ENDTRANSFER + resource re-issue + re-arm of the
///       same data TRB (the response buffer still holds the data)
///   4 = state Status: the status ZLP was lost - re-arm the status
pub fn rescue_read64() -> u32 {
    unsafe {
        harvest_trace_outcome();
        let last = TRACE_HARVEST_LAST_SETUP;
        if last == 0xFFFF_FFFF || (last >> 16) != 6 || (last & 0xffff) != 64 {
            return 0;
        }
        let dsts = read(DSTS);
        if dsts & DSTS_DEVCTRLHLT != 0 || (dsts >> 18) & 0xf != 0 {
            return 0;
        }
        let setup = ep0_setup_data_ptr();
        cache_invalidate(setup as usize, 8);
        let mut latched = false;
        for offset in 0..8 {
            if read_volatile(setup.add(offset)) != 0 {
                latched = true;
                break;
            }
        }
        if latched {
            // Mirror the fresh_setup path: the latched SETUP overrides any
            // stale phase.
            EP0_STATE = Ep0State::Setup;
            handle_setup();
            return 1;
        }
        match EP0_STATE {
            Ep0State::Setup => {
                let _ = rearm_setup();
                2
            }
            Ep0State::Data => {
                let _ = end_transfer(1);
                let _ = set_transfer_resource(1);
                let trb_index = ep0_trb_index(1);
                let mut queued = start_transfer(1, ep0_trb_ptr(trb_index));
                if !queued {
                    for _ in 0..50 {
                        super::timer::delay_us(200);
                        if start_transfer(1, ep0_trb_ptr(trb_index)) {
                            queued = true;
                            break;
                        }
                    }
                }
                let _ = queued;
                3
            }
            Ep0State::Status => {
                let endpoint = if CONTROL_HAS_DATA && CONTROL_IN { 0 } else { 1 };
                let _ = start_status(endpoint);
                4
            }
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

// qpr1's dwc3_send_gadget_ep_cmd() uses a 3000-read completion budget for
// every endpoint command, including STARTTRANSFER. Keep that source contract
// here; the former 50,000-read extension was an unverified probe workaround
// and can hide a command-engine stall during the handoff.
const DWC3_EP_COMMAND_TIMEOUT: u32 = 3_000;

#[inline]
fn gsi_transfer_params(event_buffer: u32, trb: usize) -> Option<(u32, u32)> {
    let count = super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count;
    if event_buffer == 0 || event_buffer > count || trb & 0x3f != 0 {
        return None;
    }
    Some((
        GSI_TRB_ADDR_BIT_53 | GSI_TRB_ADDR_BIT_55 | (event_buffer << GSI_EVENT_ADDR_INDEX_SHIFT),
        trb as u32,
    ))
}

/// Set up the Qualcomm GSI event-buffer ABI before any GSI endpoint can be
/// started. Android allocates three additional event buffers and marks them
/// with both the GSI enable/index bits in GEVNTADRHI and the interrupt-mask
/// bit in GEVNTCOUNT. EP0 continues to use event buffer zero.
unsafe fn configure_gsi_event_buffers() -> bool {
    let resources = super::platform::bramble::usb_resources();
    let gsi = resources.gsi;
    unsafe {
        let mut general = read_qscratch(gsi.general_cfg_offset);
        general |= GSI_CLK_EN;
        write_qscratch(gsi.general_cfg_offset, general);
        general |= GSI_RESTART_DBL_PNTR;
        write_qscratch(gsi.general_cfg_offset, general);
        general &= !GSI_RESTART_DBL_PNTR;
        write_qscratch(gsi.general_cfg_offset, general);
        if read_qscratch(gsi.general_cfg_offset) & GSI_CLK_EN == 0 {
            return false;
        }

        for index in 0..gsi.event_buffer_count as usize {
            let event = addr_of_mut!(GSI_EVENTS).cast::<EventBuffer>().add(index);
            let event_address = event as usize as u64;
            cache_clean(event as usize, EVENT_BUFFER_SIZE);
            let register = GEVNTADRLO0 + (index + 1) * GEVNT_BUFFER_STRIDE;
            write(register, event_address as u32);
            write(
                register + 4,
                (event_address >> 32) as u32
                    | (((index + 1) as u32) << GSI_EVENT_ADDR_EN_SHIFT)
                    | (((index + 1) as u32) << GSI_EVENT_ADDR_INDEX_SHIFT),
            );
            write(register + 8, EVENT_BUFFER_SIZE as u32);
            write(register + 12, GSI_EVENT_INTR_MASK);
        }
    }
    true
}

/// Enable the GSI wrapper at the point Android starts a GSI endpoint. Keeping
/// this separate from event-buffer allocation avoids asserting GSI_EN for a
/// normal gadget that has no IPA/GSI channel.
unsafe fn enable_gsi_wrapper() -> bool {
    let offset = super::platform::bramble::usb_resources()
        .gsi
        .general_cfg_offset;
    unsafe {
        let mut value = read_qscratch(offset);
        value |= GSI_CLK_EN;
        write_qscratch(offset, value);
        value |= GSI_EN;
        write_qscratch(offset, value);
        read_qscratch(offset) & GSI_EN != 0
    }
}

const GSI_MAX_RING_TRBS: usize = 10;

/// Build the circular DWC3 TRB ring consumed by Qualcomm's GSI wrapper. The
/// ring is caller-owned DMA memory, while buffer addresses are the contiguous
/// request pool supplied by the IPA/GSI client. This mirrors Android's
/// `gsi_prepare_trbs()` split between ring allocation and buffer storage.
unsafe fn prepare_gsi_ring(
    event_index: usize,
    endpoint: usize,
    ring_base: u64,
    buffer_base: usize,
    buffer_length: usize,
) -> bool {
    let in_direction = endpoint & 1 != 0;
    let Some(shape) = gsi_ring_shape(in_direction, GSI_DEFAULT_NUM_BUFFERS) else {
        return false;
    };
    let pool = super::platform::bramble::usb_resources().dma_pool;
    let ring_bytes = shape.num_trbs.saturating_mul(core::mem::size_of::<Trb>());
    let buffer_bytes = (shape.data_trbs as u64).saturating_mul(buffer_length as u64);
    if shape.num_trbs > GSI_MAX_RING_TRBS
        || !super::platform::bramble::dma_region_valid(pool, ring_base, ring_bytes as u64, 0x400)
        || !super::platform::bramble::dma_region_valid(pool, buffer_base as u64, buffer_bytes, 64)
        || buffer_length == 0
    {
        return false;
    }

    unsafe {
        let ring = ring_base as usize as *mut Trb;
        for index in 0..shape.num_trbs {
            let mut trb = Trb::default();
            if index == shape.num_trbs - 1 {
                // The GSI wrapper uses the same address[55:53] and
                // interrupter-index encoding as STARTTRANSFER.
                trb.bpl = ring_base as u32;
                trb.bph = (ring_base >> 32) as u32
                    | GSI_TRB_ADDR_BIT_53
                    | GSI_TRB_ADDR_BIT_55
                    | ((event_index as u32 + 1) << GSI_EVENT_ADDR_INDEX_SHIFT);
                trb.ctrl = TRB_LINK | TRB_HWO;
            } else if in_direction {
                // The first n+1 entries are deliberate zero-length normal
                // TRBs (ZLPs); the following n entries point at the
                // contiguous buffer pool. Android leaves HWO clear here and
                // lets the GSI path own the buffer progression.
                if index >= shape.first_buffer_trb {
                    let buffer_index = index - shape.first_buffer_trb;
                    let address = buffer_base
                        .saturating_add(buffer_index.saturating_mul(buffer_length))
                        as u64;
                    trb.bpl = address as u32;
                    trb.bph = (address >> 32) as u32;
                }
                trb.ctrl = TRB_NORMAL | TRB_IOC;
            } else if index == 0 {
                // The Bramble Android OUT ring starts with a link to the
                // second TRB, then closes with another link TRB.
                let next = ring_base + core::mem::size_of::<Trb>() as u64;
                trb.bpl = next as u32;
                trb.bph = (next >> 32) as u32;
                trb.ctrl = TRB_LINK;
            } else {
                let buffer_index = index - 1;
                let address =
                    buffer_base.saturating_add(buffer_index.saturating_mul(buffer_length)) as u64;
                trb.bpl = address as u32;
                trb.bph = (address >> 32) as u32;
                trb.size = buffer_length as u32;
                // OUT HWO is set by UPDATETRANSFER, matching Android's
                // lifecycle. Preparing a ring must not make it live early.
                trb.ctrl = TRB_NORMAL | TRB_IOC | TRB_CSP | TRB_ISP_IMI;
            }
            write_volatile(ring.add(index), trb);
        }
        cache_clean(
            ring_base as usize,
            shape.num_trbs * core::mem::size_of::<Trb>(),
        );
    }
    true
}

/// Publish the ring and doorbell addresses consumed by the IPA/GSI channel
/// setup, and prepare the complete circular TRB layout. Android does this
/// after endpoint configuration and before starting the channel; a normal
/// UDC endpoint therefore never writes to an unowned doorbell by accident.
pub unsafe fn configure_gsi_channel(
    endpoint: usize,
    event_buffer: u32,
    ring_base: u64,
    doorbell: u64,
) -> bool {
    // Do not retain the old incomplete ABI as a fake successful setup.
    // A GSI channel is meaningful only when the caller supplies the actual
    // contiguous request pool consumed by gsi_prepare_trbs().
    let _ = (endpoint, event_buffer, ring_base, doorbell);
    false
}

/// Configure one Qualcomm GSI channel with its complete DMA ownership.
/// `buffer_base..buffer_base + 4 * buffer_length` is the contiguous request
/// pool corresponding to Android's `gsi_prepare_trbs()` layout.  Both the
/// TRB ring and that pool must be in the DT-declared Apps-SMMU IOVA window.
pub unsafe fn configure_gsi_channel_with_buffers(
    endpoint: usize,
    event_buffer: u32,
    ring_base: u64,
    doorbell: u64,
    buffer_base: u64,
    buffer_length: usize,
) -> bool {
    let resources = super::platform::bramble::usb_resources();
    let count = resources.gsi.event_buffer_count.min(3);
    if endpoint < 2
        || event_buffer == 0
        || event_buffer > count
        || ring_base == 0
        || ring_base & 0x3ff != 0
        || doorbell == 0
        || doorbell & 0x3 != 0
        || doorbell >> 32 != 0
        || buffer_base > usize::MAX as u64
        || buffer_length == 0
    {
        return false;
    }
    let index = (event_buffer - 1) as usize;
    unsafe {
        if !prepare_gsi_ring(
            index,
            endpoint,
            ring_base,
            buffer_base as usize,
            buffer_length,
        ) {
            return false;
        }
        write_qscratch(
            resources.gsi.ring_base_low_offset + index * 4,
            ring_base as u32,
        );
        write_qscratch(
            resources.gsi.ring_base_high_offset + index * 4,
            (ring_base >> 32) as u32,
        );
        write_qscratch(
            resources.gsi.doorbell_low_offset + index * 4,
            doorbell as u32,
        );
        write_qscratch(
            resources.gsi.doorbell_high_offset + index * 4,
            (doorbell >> 32) as u32,
        );
        GSI_CHANNEL_ENDPOINT[index] = endpoint;
        GSI_CHANNEL_READY[index] = true;
        GSI_RING_BASES[index] = ring_base;
        GSI_RING_TRB_COUNTS[index] = gsi_ring_shape(endpoint & 1 != 0, GSI_DEFAULT_NUM_BUFFERS)
            .map(|shape| shape.num_trbs)
            .unwrap_or(0);
        GSI_BUFFER_BASES[index] = buffer_base;
        GSI_BUFFER_LENGTHS[index] = buffer_length;
        GSI_DOORBELL_BASES[index] = doorbell;
        GSI_RESOURCE_INDEX[index] = 0;
        GSI_RING_ACTIVE[index] = false;
    }
    true
}

/// Allocate and configure a complete GSI channel from the active USB DMA
/// pool. This is the path used by a real gadget client; callers no longer
/// need to invent physical addresses for the ring or request buffers.
pub unsafe fn allocate_gsi_channel(
    endpoint: usize,
    event_buffer: u32,
    doorbell: u64,
    buffer_length: usize,
) -> Option<(*mut u8, *mut u8)> {
    let shape = gsi_ring_shape(endpoint & 1 != 0, GSI_DEFAULT_NUM_BUFFERS)?;
    let ring_bytes = shape.num_trbs.checked_mul(core::mem::size_of::<Trb>())?;
    let buffer_bytes = shape.data_trbs.checked_mul(buffer_length)?;
    let ring = unsafe { allocate_usb_dma(ring_bytes, 0x400)? };
    let buffers = unsafe { allocate_usb_dma(buffer_bytes, 64)? };
    if unsafe {
        !configure_gsi_channel_with_buffers(
            endpoint,
            event_buffer,
            ring as usize as u64,
            doorbell,
            buffers as usize as u64,
            buffer_length,
        )
    } {
        return None;
    }
    Some((ring, buffers))
}

/// Ring the physical doorbell supplied by the IPA/GSI client. The Android
/// glue writes the address of the ring's final link TRB as two 32-bit MMIO
/// stores; it does not ring the DWC3 QSCRATCH register itself.
unsafe fn ring_gsi_doorbell(index: usize) -> bool {
    if index >= 3 {
        return false;
    }
    let doorbell = unsafe { GSI_DOORBELL_BASES[index] };
    let ring = unsafe { GSI_RING_BASES[index] };
    let count = unsafe { GSI_RING_TRB_COUNTS[index] };
    if doorbell == 0 || ring == 0 || count == 0 {
        return false;
    }
    let Some(link_offset) = (count - 1).checked_mul(core::mem::size_of::<Trb>()) else {
        return false;
    };
    let Some(link) = ring.checked_add(link_offset as u64) else {
        return false;
    };
    if !super::platform::bramble::dma_region_valid(
        super::platform::bramble::usb_resources().dma_pool,
        link,
        core::mem::size_of::<Trb>() as u64,
        64,
    ) {
        return false;
    }
    unsafe {
        // DWC3's GSI link TRB carries the interrupter/address-extension bits,
        // but the IPA doorbell receives the plain DMA address of that TRB.
        let db = doorbell as usize as *mut u32;
        let db_hi = doorbell.saturating_add(4) as usize as *mut u32;
        core::ptr::write_volatile(db, link as u32);
        let _ = core::ptr::read_volatile(db);
        core::ptr::write_volatile(db_hi, (link >> 32) as u32);
        let _ = core::ptr::read_volatile(db_hi);
    }
    true
}

/// Block or release the GSI write doorbell. Qualcomm runtime suspend blocks
/// writes, waits for IF_STS to idle, then halts DWC3 and drops the platform
/// vote in that order.
pub unsafe fn set_gsi_doorbell_blocked(blocked: bool) -> bool {
    let offset = super::platform::bramble::usb_resources()
        .gsi
        .general_cfg_offset;
    unsafe {
        let mut value = read_qscratch(offset);
        if blocked {
            value |= GSI_BLOCK_WR_GO;
        } else {
            value &= !GSI_BLOCK_WR_GO;
        }
        write_qscratch(offset, value);
        (read_qscratch(offset) & GSI_BLOCK_WR_GO != 0) == blocked
    }
}

unsafe fn gsi_ready_to_suspend() -> bool {
    let offset = super::platform::bramble::usb_resources()
        .gsi
        .interface_status_offset;
    unsafe {
        for _ in 0..1500 {
            if read_qscratch(offset) & GSI_WR_CTRL_STATE == 0 {
                return true;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
    }
    false
}

unsafe fn cache_clean(address: usize, length: usize) {
    // DWC3 and the Apps SMMU consume these objects by DMA.  The probe may be
    // entered with the bootloader's caches enabled, so a no-op here would
    // leave the freshly written TRB/page table only in the CPU cache.  The
    // explicit A/B must force the maintenance even when the DT describes a
    // coherent GSI path; otherwise --dma-cache-maintenance silently compiles
    // but is not an experiment at all.
    let force_cache_maintenance = cfg!(fullerene_aarch64_usb_dma_cache_maintenance);
    if !super::platform::bramble::usb_resources()
        .gsi
        .disable_io_coherency
        && !force_cache_maintenance
    {
        unsafe { core::arch::asm!("dsb sy", options(nostack)) };
        return;
    }
    let start = address & !63;
    let end = address.saturating_add(length).saturating_add(63) & !63;
    let mut line = start;
    while line < end {
        unsafe { core::arch::asm!("dc cvac, {address}", address = in(reg) line, options(nostack)) };
        line += 64;
    }
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
}

unsafe fn cache_invalidate(address: usize, length: usize) {
    // Keep the invalidate side of the explicit DMA A/B symmetrical with
    // cache_clean(): observing controller-owned event/TRB writes also needs
    // the cache-line operation when the DT path advertises I/O coherency.
    let force_cache_maintenance = cfg!(fullerene_aarch64_usb_dma_cache_maintenance);
    if !super::platform::bramble::usb_resources()
        .gsi
        .disable_io_coherency
        && !force_cache_maintenance
    {
        unsafe { core::arch::asm!("dsb sy", options(nostack)) };
        return;
    }
    let start = address & !63;
    let end = address.saturating_add(length).saturating_add(63) & !63;
    let mut line = start;
    while line < end {
        unsafe { core::arch::asm!("dc ivac, {address}", address = in(reg) line, options(nostack)) };
        line += 64;
    }
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
}

/// Restore the `GUSB2PHYCFG0` bits that `send_ep_command_result` cleared for the
/// duration of an endpoint command.
///
/// Linux performs this restore immediately before `return` in
/// `dwc3_send_gadget_ep_cmd()` (`gadget.c:501-503`), so it runs on EVERY exit -
/// success, timeout and error alike. Fullerene had it on the timeout path only,
/// which left `SUSPHY` and `ENBLSLPM` cleared after the *successful* endpoint
/// commands the handoff actually issues. Measured: `GUSB2PHYCFG0.SUSPHY` read 0
/// at the post-run readout, after all EP0 traffic had finished - and the source
/// order in `init_usb2_gadget_reuse_fastboot_ep0` sets it at 7122, after every
/// other writer. One helper for both exits, so the two paths cannot drift apart
/// again the way they did between the original success-only bug and today's
/// timeout-only fix.
#[inline]
unsafe fn restore_usb2_command_guard(saved_usb2_config: u32) {
    if saved_usb2_config != 0 {
        let usb2 = read(GUSB2PHYCFG0);
        mark_g2w_site(1001);
        write(GUSB2PHYCFG0, usb2 | saved_usb2_config);
    }
}

unsafe fn send_ep_command_result(
    endpoint: usize,
    command: u32,
    param0: u32,
    param1: u32,
    param2: u32,
) -> Option<u8> {
    trace_event(
        TRACE_EP_COMMAND_ISSUE,
        command,
        endpoint as u32,
        param0,
        param1,
        param2,
    );
    let mut saved_usb2_config = 0;
    unsafe {
        // The DWC3 programming guide requires SUSPENDUSB2 and ENBLSLPM to be
        // clear while issuing endpoint commands at USB2 speeds. Linux does
        // this in dwc3_send_gadget_ep_cmd(); a Fastboot handoff commonly
        // leaves one or both bits set after tearing down its gadget.
        let command_kind = command & 0x0f;
        if cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_cmd_guard)
            || command_kind == DEPCMD_ENDTRANSFER
            || read(DSTS) & DSTS_CONNECTSPD_MASK != DSTS_SUPERSPEED
        {
            let mut usb2 = read(GUSB2PHYCFG0);
            saved_usb2_config = usb2 & (GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
            if saved_usb2_config != 0 {
                usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
                mark_g2w_site(1002);
                write(GUSB2PHYCFG0, usb2);
                let _ = read(GUSB2PHYCFG0);
                CMD_GUARD_ENGAGED = true;
            }
        }
        // The DWC3 register names are counter-intuitive: PAR2 is at +0x00,
        // PAR1 at +0x04, and PAR0 at +0x08. Keep both the software argument
        // order and the MMIO write order identical to Linux's
        // dwc3_send_gadget_ep_cmd(). Factory ABL is a useful A/B here: its
        // endpoint-command helper writes only the parameter registers used by
        // the command kind (PAR1/PAR0 for SETEPCONFIG and STARTTRANSFER,
        // PAR0 for SETTRANSFRESOURCE, and none for the other commands). It
        // never unconditionally clears PAR2. Preserve the Linux form by
        // default, but allow the binary-derived write mask to be tested
        // without changing the normal handoff.
        #[cfg(fullerene_aarch64_usb_abl_command_params)]
        match command_kind {
            DEPCMD_SETEPCONFIG | DEPCMD_STARTTRANSFER => {
                write(dep_reg(endpoint, 0x04), param1);
                write(dep_reg(endpoint, 0x08), param0);
            }
            DEPCMD_SETTRANSFRESOURCE => {
                write(dep_reg(endpoint, 0x08), param0);
            }
            _ => {}
        }
        #[cfg(not(fullerene_aarch64_usb_abl_command_params))]
        {
            write(dep_reg(endpoint, 0x08), param0);
            write(dep_reg(endpoint, 0x04), param1);
            write(dep_reg(endpoint, 0x00), param2);
        }
        // Linux's writel() provides the MMIO ordering barrier that separates
        // the parameter writes from the command latch. Preserve that ordering
        // explicitly in this freestanding Rust path.
        core::arch::asm!("dsb sy", options(nostack));
        write(dep_reg(endpoint, 0x0c), command | DEPCMD_CMDACT);
    }
    // qpr1's dwc3_send_gadget_ep_cmd() uses a bounded 3,000-read polling
    // window. Keep this tight: a command that never retires must not leave
    // the early handoff spending an architecture-dependent amount of time in
    // a NOP loop while the host waits for EP0.
    for _ in 0..DWC3_EP_COMMAND_TIMEOUT {
        let status = unsafe { read(dep_reg(endpoint, 0x0c)) };
        if status & DEPCMD_CMDACT == 0 {
            trace_event(
                TRACE_EP_COMMAND_DONE,
                command,
                endpoint as u32,
                status,
                0,
                unsafe { read(DSTS) },
            );
            let success = status & 0xf000 == 0;
            let resource_index = ((status >> DEPCMD_PARAM_SHIFT) & 0x7f) as u8;
            if endpoint == 0 && command & 0x0f == DEPCMD_STARTTRANSFER {
                unsafe {
                    SETUP_ARM_LAST_COMMAND = status;
                }
            }
            restore_usb2_command_guard(saved_usb2_config);
            return success.then_some(resource_index);
        }
        unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
    }
    // Restore on the TIMEOUT path too.
    //
    // Linux puts this restore immediately before `return` in
    // `dwc3_send_gadget_ep_cmd()` (`gadget.c:501-503`), i.e. on EVERY exit -
    // timeout, error and success alike. Fullerene had it inside the success
    // branch only, so a timed-out endpoint command left GUSB2PHYCFG with
    // SUSPHY and ENBLSLPM still cleared. Measured: the `susphy` CCS word reads
    // 0 at the post-run readout, after all EP0 traffic has finished, which is
    // exactly this bug showing itself.
    //
    // The DWC3 programming guide (3.30a / 3.31a section 3.2.2) requires both
    // bits clear *while issuing* the command and restored afterwards; leaving
    // SUSPHY clear parks the USB2 PHY in its suspend configuration, where the
    // parallel receive path can stop - a candidate mechanism for a device that
    // attaches and then receives nothing.
    restore_usb2_command_guard(saved_usb2_config);
    trace_event(
        TRACE_EP_COMMAND_TIMEOUT,
        command,
        endpoint as u32,
        unsafe { read(dep_reg(endpoint, 0x0c)) },
        0,
        unsafe { read(DSTS) },
    );
    if endpoint == 0 && command & 0x0f == DEPCMD_STARTTRANSFER {
        unsafe {
            SETUP_ARM_LAST_COMMAND = 0x8000_0000;
        }
    }
    log_puts("usb: DWC3 endpoint command timeout\n");
    None
}

#[inline]
unsafe fn send_ep_command(
    endpoint: usize,
    command: u32,
    param0: u32,
    param1: u32,
    param2: u32,
) -> bool {
    unsafe { send_ep_command_result(endpoint, command, param0, param1, param2).is_some() }
}

/// Allocate one DWC3 transfer resource for an endpoint.
///
/// The SETTRANSFRESOURCE completion does not provide the transfer index used
/// by STARTTRANSFER. Linux obtains that index from the STARTTRANSFER
/// completion (GETTRANSFERINDEX in the EP0 path) and retains it for
/// UPDATETRANSFER/ENDTRANSFER.
unsafe fn set_transfer_resource(endpoint: usize) -> bool {
    unsafe { send_ep_command_result(endpoint, DEPCMD_SETTRANSFRESOURCE, 1, 0, 0).is_some() }
}

unsafe fn configure_endpoint(endpoint: usize, max_packet: u32, modify: bool) -> bool {
    unsafe { configure_endpoint_kind(endpoint, max_packet, DEPCFG_EP_TYPE_CONTROL, modify) }
}

unsafe fn configure_endpoint_kind(
    endpoint: usize,
    max_packet: u32,
    endpoint_type: u32,
    modify: bool,
) -> bool {
    unsafe {
        configure_endpoint_kind_with_interrupter(endpoint, max_packet, endpoint_type, modify, 0)
    }
}

unsafe fn configure_endpoint_kind_with_interrupter(
    endpoint: usize,
    max_packet: u32,
    endpoint_type: u32,
    modify: bool,
    interrupter: u32,
) -> bool {
    if !unsafe {
        configure_endpoint_config(endpoint, max_packet, endpoint_type, modify, interrupter)
    } {
        return false;
    }
    // Linux allocates a transfer resource immediately after configuring each
    // endpoint. DEPSTARTCFG only resets the allocation window; issuing
    // SETTRANSFRESOURCE for every possible endpoint is not equivalent and can
    // make the handoff fail before the first pull-up.
    if !modify
        && !cfg!(fullerene_aarch64_usb_gadget_handoff_no_transfer_resource)
        && !cfg!(fullerene_aarch64_usb_gadget_handoff_android_resource_order)
    {
        return unsafe { set_transfer_resource(endpoint) };
    }
    true
}

unsafe fn configure_endpoint_config(
    endpoint: usize,
    max_packet: u32,
    endpoint_type: u32,
    modify: bool,
    interrupter: u32,
) -> bool {
    let action = if modify { DEPCMD_ACTION_MODIFY } else { 0 };
    let mut param0 = action | endpoint_type | (max_packet << DEPCFG_MAX_PACKET_SHIFT);
    // Match dwc3_gadget_set_ep_config(): control endpoints request both
    // transfer-complete and transfer-not-ready notifications, while ordinary
    // data endpoints request transfer-in-progress and transfer-not-ready.
    // EP0's NRDY event is the controller's notification that it has accepted
    // the host SETUP phase, so suppressing it changes the control state
    // machine even though the first transfer is queued successfully.
    let mut param1 = if endpoint_type == DEPCFG_EP_TYPE_CONTROL {
        DEPCFG_XFER_COMPLETE_EN
    } else {
        DEPCFG_XFER_IN_PROGRESS_EN
    };
    let abl_ep_config = cfg!(fullerene_aarch64_usb_abl_ep_config) && endpoint <= 1 && !modify;
    if abl_ep_config {
        // Factory Bramble ABL's DwcConfigureEP follows the Qualcomm msm
        // DEPCFG contract: burst size 3, FIFO number on IN endpoints, and
        // endpoint address (not logical endpoint number) in P1. Its EP0
        // notification mask is XFER_COMPLETE|XFER_IN_PROGRESS (0x300), with
        // no XferNotReady bit. The previous A/B used a misread raw pair;
        // keep this flag tied to the disassembled instruction sequence.
        param0 = endpoint_type
            | (max_packet << DEPCFG_MAX_PACKET_SHIFT)
            | (3 << DEPCFG_BURST_SIZE_SHIFT);
        if endpoint & 1 != 0 {
            param0 |= ((endpoint / 2) as u32) << DEPCFG_FIFO_NUMBER_SHIFT;
        }
        param1 = DEPCFG_XFER_COMPLETE_EN
            | DEPCFG_XFER_IN_PROGRESS_EN
            | (interrupter & 0x1f) << DEPCFG_INT_NUM_SHIFT
            | (endpoint as u32) << DEPCFG_EP_NUMBER_SHIFT;
    } else {
        #[cfg(fullerene_aarch64_usb_gadget_handoff_xbl_ep0_config)]
        if endpoint <= 1 && endpoint_type == DEPCFG_EP_TYPE_CONTROL {
            // Stock Bramble XBL's fixed EP0 configuration emits P1=0x300:
            // XFER_COMPLETE_EN | XFER_IN_PROGRESS_EN. This is a binary-derived
            // A/B for the XBL function-driver contract; keep the Linux-default
            // NRDY notification in the generic path.
            param1 |= DEPCFG_XFER_IN_PROGRESS_EN;
        }
        if endpoint <= 1
            && !(cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_ep0_config)
                && endpoint_type == DEPCFG_EP_TYPE_CONTROL)
        {
            param1 |= DEPCFG_XFER_NOT_READY_EN;
        }
        param1 |= (interrupter & 0x1f) << DEPCFG_INT_NUM_SHIFT;
        param1 |= (endpoint as u32) << DEPCFG_EP_NUMBER_SHIFT;
    }
    let param2 = 0;
    unsafe { send_ep_command(endpoint, DEPCMD_SETEPCONFIG, param0, param1, param2) }
}

#[inline]
unsafe fn apply_ep0_txfifo_fix() {
    #[cfg(fullerene_aarch64_usb_gadget_handoff_ep0_txfifo_fix)]
    {
        // Handshakes are generated internally, but every EP0 IN data packet
        // is pushed through the endpoint's TX FIFO. A Fastboot session that
        // resized or emptied FIFO 0 leaves EP1 IN unable to send any data
        // packet: the SETUP handshake still works and the host's descriptor
        // read then NAKs forever (read/64 -110). Raise a degenerate depth
        // while preserving the FIFO start address.
        let fifo = read(GTXFIFOSIZ0);
        let depth = fifo & 0x7fff;
        if depth < 32 {
            let raised = (fifo & 0xffff_0000) | 32;
            write(GTXFIFOSIZ0, raised);
            trace_event(
                TRACE_SETUP_QUEUED,
                0x5458_4631, // "TXF1" EP0 IN FIFO raised
                fifo,
                raised,
                0,
                read(DSTS),
            );
            let _ = read(GTXFIFOSIZ0);
        }
    }
}

unsafe fn start_transfer(endpoint: usize, trb: *const Trb) -> bool {
    let address = unsafe { dma_iova_for(trb as usize) };
    unsafe {
        // DWC3's STARTTRANSFER parameters are PAR0=address[63:32] and
        // PAR1=address[31:0]. The endpoint command helper writes the named
        // param0/param1 fields to those registers respectively. Linux issues
        // STARTTRANSFER with command parameter 0 for EP0 and ordinary
        // non-isochronous endpoints; the controller returns the resource
        // index in the command completion, which is retained below.
        let Some(resource_index) = send_ep_command_result(
            endpoint,
            DEPCMD_STARTTRANSFER,
            (address >> 32) as u32,
            address as u32,
            0,
        ) else {
            return false;
        };
        if endpoint < 2 {
            EP0_RESOURCE_INDEX[endpoint] = resource_index;
        } else if endpoint < 4 {
            DATA_RESOURCE_INDEX[endpoint - 2] = resource_index;
        }
        true
    }
}

/// Retry an EP0 Start Transfer for up to `window_ms`.
///
/// The endpoint command engine rejects (or wedges) Start Transfer for a
/// bounded window after the host's bus reset, while the identical command
/// succeeds seconds later. The host keeps issuing IN tokens during the data
/// phase and tolerates the NAKs until its 5 s control timeout, so a retry
/// window measured in seconds still lands inside the host's first read
/// instead of stalling the transfer and losing the whole enumeration.
///
/// A failed re-arm can also mean the previous owner's transfer is still
/// active and its transfer resource is therefore consumed. msm-4.19 revokes
/// every active transfer with End Transfer and waits 100 us on DWC_usb31
/// before the resource is reusable; repeat that revocation between attempts
/// instead of assuming the bus reset already flushed the transfer.
unsafe fn retry_start_transfer(endpoint: usize, trb: *const Trb, window_ms: u64) -> bool {
    let deadline = unsafe {
        arch_counter().saturating_add(arch_counter_frequency().saturating_mul(window_ms) / 1000)
    };
    loop {
        if unsafe { start_transfer(endpoint, trb) } {
            return true;
        }
        if unsafe { arch_counter() } >= deadline {
            return false;
        }
        unsafe {
            let resource_index = if endpoint < 2 {
                let index = EP0_RESOURCE_INDEX[endpoint];
                if index == 0 { 1 } else { index }
            } else {
                1
            };
            send_ep_command(
                endpoint,
                DEPCMD_ENDTRANSFER
                    | DEPCMD_CMDIOC
                    | DEPCMD_HIPRI_FORCERM
                    | ((resource_index as u32) << DEPCMD_PARAM_SHIFT),
                0,
                0,
                0,
            );
        }
        super::timer::delay_us(200);
        unsafe {
            set_transfer_resource(endpoint);
        }
        super::timer::delay_us(300);
    }
}

unsafe fn end_transfer(endpoint: usize) -> bool {
    // NOTE(bisect): the EP0-OUT index-0 rewrite is temporarily restored.
    // Passing the legitimate resource index 0 wedged the rescue path on the
    // handset (gate runs stopped reaching evaluation); re-derive the correct
    // form from a passing baseline before reapplying.
    let resource_index = if endpoint < 2 {
        let index = unsafe { EP0_RESOURCE_INDEX[endpoint] };
        if index == 0 { 1 } else { index }
    } else if endpoint < 4 {
        let index = unsafe { DATA_RESOURCE_INDEX[endpoint - 2] };
        if index == 0 { 1 } else { index }
    } else {
        1
    };
    unsafe {
        send_ep_command(
            endpoint,
            DEPCMD_ENDTRANSFER
                | DEPCMD_HIPRI_FORCERM
                | ((resource_index as u32) << DEPCMD_PARAM_SHIFT),
            0,
            0,
            0,
        )
    }
}

/// Apply qpr1's active-transfer part of `dwc3_gadget_reset_interrupt()` to
/// the control endpoint.  A bus reset terminates the wire transaction, but
/// qpr1 still revokes the DWC3 transfer resource before clearing endpoint
/// stalls.  Keep this opt-in because the normal handoff profile deliberately
/// preserves its armed EP0 across reset; the source-order reset A/B uses the
/// returned STARTTRANSFER resource index when it is available.
unsafe fn stop_active_ep0_at_reset() -> bool {
    unsafe {
        let resource_index = EP0_RESOURCE_INDEX[0];
        if resource_index == 0 {
            return true;
        }
        let stopped = send_ep_command(
            0,
            DEPCMD_ENDTRANSFER
                | DEPCMD_CMDIOC
                | DEPCMD_HIPRI_FORCERM
                | ((resource_index as u32) << DEPCMD_PARAM_SHIFT),
            0,
            0,
            0,
        );
        EP0_RESOURCE_INDEX[0] = 0;
        // qpr1 waits 100us after ENDTRANSFER on DWC_usb31 because the
        // hardware cannot provide the older command-completion guarantee.
        if read(GSNPSID) >> 16 == DWC31_IP {
            crate::timer::delay_us(100);
        }
        stopped
    }
}

/// Apply the hardware portion of the official Qualcomm DWC3 stop cleanup.
///
/// `dwc3_gadget_run_stop(false, false)` acknowledges the GSI event buffers
/// and then sends `DWC3_CONTROLLER_NOTIFY_CLEAR_DB`; the msm glue blocks GSI
/// write-go and clears GSI_EN. The active-transfer helper is intentionally
/// not folded in here: Linux supplies its per-endpoint resource index from
/// `struct dwc3_ep`, which a fresh fastboot handoff does not inherit.
pub(super) unsafe fn clear_gsi_stop_state() {
    unsafe {
        let gsi = super::platform::bramble::usb_resources().gsi;
        for index in 0..gsi.event_buffer_count.min(3) as usize {
            let register = GEVNTCOUNT0 + (index + 1) * GEVNT_BUFFER_STRIDE;
            let count = read(register) & GEVNTCOUNT_MASK;
            write(register, count);
        }

        clear_gsi_doorbell_state();
    }
}

/// Reproduce Qualcomm's DWC3_CONTROLLER_NOTIFY_CLEAR_DB notification.
///
/// The Android msm callback only performs this write when its GSI event
/// buffers exist: block new GSI doorbells, then clear GSI_EN. It does not
/// touch EP0, the USB2 PHY, or the primary DWC3 event ring.
pub(super) unsafe fn clear_gsi_doorbell_state() {
    unsafe {
        let offset = super::platform::bramble::usb_resources()
            .gsi
            .general_cfg_offset;
        let mut value = read_qscratch(offset) | GSI_BLOCK_WR_GO;
        write_qscratch(offset, value);
        value = read_qscratch(offset) & !GSI_EN;
        write_qscratch(offset, value);
    }
}

/// Revoke every ordinary UDC data transfer before endpoint state or request
/// ownership is reset. EP0 is handled by the control-reset path separately.
unsafe fn teardown_data_endpoints() {
    unsafe {
        if !DATA_ENDPOINTS_READY {
            return;
        }
        for endpoint in 2..=3 {
            if DATA_RESOURCE_INDEX[endpoint - 2] != 0 {
                let _ = end_transfer(endpoint);
            }
        }
        write(DALEPENA, read(DALEPENA) & !((1 << 2) | (1 << 3)));
        let _ = udc_mut().disable_endpoint(0x02);
        let _ = udc_mut().disable_endpoint(0x83);
        DATA_ENDPOINTS_READY = false;
        DATA_RESOURCE_INDEX = [0; 2];
        DATA_REQUEST_SLOTS = [usize::MAX; 2];
    }
}

/// Cancel outstanding ordinary requests at the runtime-PM boundary while
/// retaining endpoint configuration for resume. DWC3 must no longer own a
/// TRB when the UDC is marked suspended.
unsafe fn suspend_data_transfers() {
    unsafe {
        if !DATA_ENDPOINTS_READY {
            return;
        }
        for endpoint in 2..=3 {
            let index = endpoint - 2;
            if DATA_RESOURCE_INDEX[index] != 0 {
                let _ = end_transfer(endpoint);
            }
            let address = if endpoint == 3 { 0x83 } else { 0x02 };
            let slot = DATA_REQUEST_SLOTS[index];
            if slot != usize::MAX {
                let length = udc_mut()
                    .request(address, slot)
                    .map(|request| request.length)
                    .unwrap_or(0);
                let _ = udc_mut().complete(address, slot, 0, true);
                GadgetDriver::on_data_complete(gadget_mut(), address, 0, true);
                let _ = udc_mut().release(address, slot);
                trace_event(TRACE_TRANSFER_COMPLETE, endpoint as u32, 0, 0, length, 1);
            }
            DATA_RESOURCE_INDEX[index] = 0;
            DATA_REQUEST_SLOTS[index] = usize::MAX;
        }
    }
}

/// Cancel live GSI requests without discarding their registered rings or
/// client doorbells. The function receives an explicit suspend callback and
/// can requeue after resume; no request is silently left owned by DWC3.
unsafe fn suspend_gsi_transfers() {
    unsafe {
        for index in 0..3 {
            if !GSI_CHANNEL_READY[index] {
                continue;
            }
            let endpoint = GSI_CHANNEL_ENDPOINT[index];
            let event_buffer = (index + 1) as u32;
            if GSI_RING_ACTIVE[index] {
                let _ = end_gsi_transfer(endpoint, event_buffer);
            }
            let address = endpoint as u8 | if endpoint & 1 != 0 { 0x80 } else { 0 };
            let slot = GSI_REQUEST_SLOTS[index];
            if slot != usize::MAX {
                GadgetDriver::on_gsi_data_complete(gadget_mut(), address, 0, true);
                let _ = udc_mut().release(address, slot);
            }
            GSI_PENDING[index] = false;
            GSI_REQUEST_SLOTS[index] = usize::MAX;
            GSI_RING_ACTIVE[index] = false;
            GSI_RESOURCE_INDEX[index] = 0;
        }
        if GSI_GADGET_BOUND {
            GadgetDriver::on_gsi_channel_suspend(gadget_mut());
        }
    }
}

/// Start a non-control transfer through Qualcomm's GSI event-buffer path.
/// event_buffer is the Android DWC3 interrupt/event-buffer index (1..=3);
/// EP0 must continue to use start_transfer and index zero.
unsafe fn start_gsi_transfer(endpoint: usize, event_buffer: u32, trb: *const Trb) -> Option<u8> {
    let Some((param0, param1)) = gsi_transfer_params(event_buffer, trb as usize) else {
        return None;
    };
    unsafe {
        if !enable_gsi_wrapper() {
            return None;
        }
        send_ep_command_result(endpoint, DEPCMD_STARTTRANSFER, param0, param1, 0)
    }
}

/// Set ownership on the OUT data TRBs and notify DWC3 of the GSI resource.
/// Android intentionally separates ring preparation from this step so a
/// channel can be armed only after its buffers and doorbell are ready.
pub unsafe fn update_gsi_transfer(endpoint: usize, event_buffer: u32) -> bool {
    let count = super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count
        .min(3);
    if endpoint < 2 || endpoint >= 8 || event_buffer == 0 || event_buffer > count {
        return false;
    }
    let index = (event_buffer - 1) as usize;
    unsafe {
        if !GSI_CHANNEL_READY[index]
            || GSI_CHANNEL_ENDPOINT[index] != endpoint
            || GSI_RING_BASES[index] == 0
            || GSI_RING_ACTIVE[index]
            || endpoint & 1 != 0
        {
            return false;
        }
        let Some(shape) = gsi_ring_shape(false, GSI_DEFAULT_NUM_BUFFERS) else {
            return false;
        };
        let ring = GSI_RING_BASES[index] as usize as *mut Trb;
        for trb_index in shape.first_buffer_trb..shape.first_buffer_trb + shape.data_trbs {
            let mut ctrl = read_volatile(addr_of!((*ring.add(trb_index)).ctrl));
            ctrl |= TRB_HWO;
            // Publish HWO behind a write barrier, as the shipped gadget.c does.
            trb_publish_barrier();
            write_volatile(addr_of_mut!((*ring.add(trb_index)).ctrl), ctrl);
        }
        cache_clean(ring as usize, shape.num_trbs * core::mem::size_of::<Trb>());
        let resource_index = GSI_RESOURCE_INDEX[index];
        if resource_index == 0
            || !send_ep_command(
                endpoint,
                DEPCMD_UPDATETRANSFER | ((resource_index as u32) << DEPCMD_PARAM_SHIFT),
                0,
                0,
                0,
            )
        {
            return false;
        }
        GSI_RING_ACTIVE[index] = true;
    }
    true
}

/// Stop a live GSI transfer before changing its ring or runtime-power state.
pub unsafe fn end_gsi_transfer(endpoint: usize, event_buffer: u32) -> bool {
    let count = super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count
        .min(3);
    if endpoint < 2 || endpoint >= 8 || event_buffer == 0 || event_buffer > count {
        return false;
    }
    let index = (event_buffer - 1) as usize;
    unsafe {
        if !GSI_CHANNEL_READY[index] || GSI_CHANNEL_ENDPOINT[index] != endpoint {
            return false;
        }
        let resource_index = GSI_RESOURCE_INDEX[index];
        if resource_index == 0 {
            return false;
        }
        let stopped = send_ep_command(
            endpoint,
            DEPCMD_ENDTRANSFER
                | DEPCMD_HIPRI_FORCERM
                | ((resource_index as u32) << DEPCMD_PARAM_SHIFT),
            0,
            0,
            0,
        );
        if stopped {
            GSI_RING_ACTIVE[index] = false;
            GSI_PENDING[index] = false;
            GSI_REQUEST_SLOTS[index] = usize::MAX;
        }
        stopped
    }
}

/// Configure a non-control bulk endpoint for the Qualcomm GSI event path.
/// This is intentionally opt-in: the normal UDC data path uses event buffer
/// zero and must not assert the global GSI enable bit merely because event
/// buffers are available.
pub unsafe fn enable_gsi_data_endpoint(
    endpoint: usize,
    event_buffer: u32,
    max_packet: u32,
) -> bool {
    let event_buffer_count = super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count;
    if endpoint < 2
        || endpoint >= 8
        || event_buffer == 0
        || event_buffer > event_buffer_count
        || max_packet == 0
    {
        return false;
    }
    let endpoint_address = endpoint as u8 | if endpoint & 1 != 0 { 0x80 } else { 0 };
    unsafe {
        if !configure_endpoint_kind_with_interrupter(
            endpoint,
            max_packet,
            DEPCFG_EP_TYPE_BULK,
            false,
            event_buffer,
        ) {
            return false;
        }
        if !udc_mut().configure_endpoint(endpoint_address, max_packet as u16, true) {
            return false;
        }
        write(DALEPENA, read(DALEPENA) | (1 << endpoint));
    }
    true
}

/// Bind a complete GSI data endpoint in the same order as the Android client:
/// configure the DWC3 endpoint, allocate the ring/request pool, publish the
/// client doorbell, then enable the wrapper. A caller receives the owned
/// request-pool pointers and can pass the first one to `queue_gsi_transfer`.
pub unsafe fn configure_gsi_data_endpoint(
    endpoint: usize,
    event_buffer: u32,
    max_packet: u32,
    doorbell: u64,
    buffer_length: usize,
) -> Option<(*mut u8, *mut u8)> {
    if !unsafe { enable_gsi_data_endpoint(endpoint, event_buffer, max_packet) } {
        return None;
    }
    let allocation =
        unsafe { allocate_gsi_channel(endpoint, event_buffer, doorbell, buffer_length) };
    if allocation.is_none() {
        let address = endpoint as u8 | if endpoint & 1 != 0 { 0x80 } else { 0 };
        unsafe {
            let _ = udc_mut().disable_endpoint(address);
            write(DALEPENA, read(DALEPENA) & !(1 << endpoint));
        }
        return None;
    }
    if !unsafe { enable_gsi_wrapper() } {
        unsafe {
            let _ = disable_gsi_data_endpoint(endpoint, event_buffer);
        }
        return None;
    }
    allocation
}

/// Tear down one GSI endpoint after its request has completed or been
/// cancelled. ENDTRANSFER precedes UDC removal, and the global wrapper is
/// disabled only once no channel remains published.
pub unsafe fn disable_gsi_data_endpoint(endpoint: usize, event_buffer: u32) -> bool {
    let count = super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count
        .min(3);
    if endpoint < 2 || endpoint >= 8 || event_buffer == 0 || event_buffer > count {
        return false;
    }
    let index = (event_buffer - 1) as usize;
    unsafe {
        if !GSI_CHANNEL_READY[index] || GSI_CHANNEL_ENDPOINT[index] != endpoint {
            return false;
        }
        if GSI_RING_ACTIVE[index] && !end_gsi_transfer(endpoint, event_buffer) {
            return false;
        }
        let address = endpoint as u8 | if endpoint & 1 != 0 { 0x80 } else { 0 };
        let _ = udc_mut().disable_endpoint(address);
        write(DALEPENA, read(DALEPENA) & !(1 << endpoint));
        GSI_PENDING[index] = false;
        GSI_REQUEST_SLOTS[index] = usize::MAX;
        GSI_RING_ACTIVE[index] = false;
        GSI_RESOURCE_INDEX[index] = 0;
        GSI_CHANNEL_READY[index] = false;
        GSI_CHANNEL_ENDPOINT[index] = 0;

        let no_channels = !GSI_CHANNEL_READY[0] && !GSI_CHANNEL_READY[1] && !GSI_CHANNEL_READY[2];
        if no_channels {
            let offset = super::platform::bramble::usb_resources()
                .gsi
                .general_cfg_offset;
            let value = read_qscratch(offset) & !GSI_EN;
            write_qscratch(offset, value);
        }
    }
    true
}

/// Queue one DMA request on a previously configured GSI data endpoint. The
/// supplied buffer is treated as the beginning of the contiguous four-buffer
/// pool expected by Android's GSI ABI; callers must provide space for all
/// four `length`-sized buffers and must not reuse it until completion.
pub unsafe fn queue_gsi_transfer(
    endpoint: usize,
    event_buffer: u32,
    buffer: *const u8,
    length: usize,
) -> bool {
    let event_buffer_count = super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count;
    if endpoint < 2
        || endpoint >= 8
        || event_buffer == 0
        || event_buffer > event_buffer_count
        || length == 0
    {
        return false;
    }
    let trb_index = (event_buffer - 1) as usize;
    let endpoint_address = endpoint as u8 | if endpoint & 1 != 0 { 0x80 } else { 0 };
    unsafe {
        if GSI_CHANNEL_ENDPOINT[trb_index] != endpoint {
            return false;
        }
        if !GSI_CHANNEL_READY[trb_index] {
            return false;
        }
        if GSI_PENDING[trb_index] {
            return false;
        }
        let Some(shape) = gsi_ring_shape(endpoint & 1 != 0, GSI_DEFAULT_NUM_BUFFERS) else {
            return false;
        };
        let total_buffer_bytes = (shape.data_trbs as u64).saturating_mul(length as u64);
        let pool = super::platform::bramble::usb_resources().dma_pool;
        if buffer as usize as u64 != GSI_BUFFER_BASES[trb_index]
            || length != GSI_BUFFER_LENGTHS[trb_index]
            || !super::platform::bramble::dma_region_valid(
                pool,
                buffer as usize as u64,
                total_buffer_bytes,
                64,
            )
        {
            return false;
        }
        let Some(request_slot) = udc_mut().queue(endpoint_address, length as u32) else {
            return false;
        };
        if !udc_mut().start(endpoint_address, request_slot) {
            let _ = udc_mut().release(endpoint_address, request_slot);
            return false;
        }
        let ring_base = GSI_RING_BASES[trb_index];
        if !prepare_gsi_ring(trb_index, endpoint, ring_base, buffer as usize, length) {
            let _ = udc_mut().release(endpoint_address, request_slot);
            return false;
        }
        GSI_PENDING[trb_index] = true;
        GSI_REQUEST_SLOTS[trb_index] = request_slot;
        let Some(resource_index) =
            start_gsi_transfer(endpoint, event_buffer, ring_base as usize as *const Trb)
        else {
            GSI_PENDING[trb_index] = false;
            GSI_REQUEST_SLOTS[trb_index] = usize::MAX;
            let _ = udc_mut().release(endpoint_address, request_slot);
            return false;
        };
        GSI_RESOURCE_INDEX[trb_index] = resource_index;
        let transfer_updated = endpoint & 1 != 0 || update_gsi_transfer(endpoint, event_buffer);
        if transfer_updated && ring_gsi_doorbell(trb_index) {
            GSI_RING_ACTIVE[trb_index] = true;
            true
        } else {
            GSI_PENDING[trb_index] = false;
            GSI_REQUEST_SLOTS[trb_index] = usize::MAX;
            let _ = end_gsi_transfer(endpoint, event_buffer);
            let _ = udc_mut().release(endpoint_address, request_slot);
            false
        }
    }
}

/// Queue an ordinary gadget bulk request on the function's EP2 OUT or EP3
/// IN endpoint. GSI is an Android IPA optimization; Linux's normal UDC path
/// still uses DWC3's event buffer zero and must remain usable independently.
pub unsafe fn queue_bulk_transfer(endpoint: usize, buffer: *const u8, length: usize) -> bool {
    if !DATA_ENDPOINTS_READY || (endpoint != 2 && endpoint != 3) || length == 0 {
        return false;
    }
    let index = endpoint - 2;
    unsafe {
        if DATA_REQUEST_SLOTS[index] != usize::MAX {
            return false;
        }
        let address = if endpoint == 3 { 0x83 } else { 0x02 };
        let pool = super::platform::bramble::usb_resources().dma_pool;
        if !super::platform::bramble::dma_region_valid(
            pool,
            buffer as usize as u64,
            length as u64,
            64,
        ) {
            return false;
        }
        let Some(slot) = udc_mut().queue(address, length as u32) else {
            return false;
        };
        if !udc_mut().start(address, slot) {
            let _ = udc_mut().release(address, slot);
            return false;
        }
        let trb = addr_of_mut!(DATA_TRBS).cast::<Trb>().add(index);
        prepare_trb_at(trb, buffer, length, TRB_NORMAL);
        DATA_REQUEST_SLOTS[index] = slot;
        if start_transfer(endpoint, trb) {
            true
        } else {
            DATA_REQUEST_SLOTS[index] = usize::MAX;
            DATA_RESOURCE_INDEX[index] = 0;
            let _ = udc_mut().release(address, slot);
            false
        }
    }
}

unsafe fn prepare_trb(index: usize, buffer: *const u8, length: usize, kind: u32) {
    let address = unsafe { dma_iova_for(buffer as usize) };
    let trb = unsafe { ep0_trb_ptr(index) };
    let flags = if cfg!(fullerene_aarch64_usb_abl_trb_flags) {
        TRB_ABL_REQUEST_FLAGS
    } else {
        TRB_HWO
            | TRB_LST
            | TRB_IOC
            | TRB_ISP_IMI
            | if cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_trb_chain) {
                TRB_CHN
            } else {
                0
            }
    };
    unsafe {
        write_volatile(addr_of_mut!((*trb).bpl), address as u32);
        write_volatile(addr_of_mut!((*trb).bph), (address >> 32) as u32);
        write_volatile(addr_of_mut!((*trb).size), length as u32);
        // The fourth DWORD carries HWO; publish it last behind a write barrier.
        trb_publish_barrier();
        write_volatile(addr_of_mut!((*trb).ctrl), kind | flags);
        cache_clean(trb as usize, core::mem::size_of::<Trb>());
    }
}

/// Prepare the EP0 SETUP TRB and, for the source-exact A/B, its separate
/// controller-owned eight-byte setup buffer.
unsafe fn prepare_ep0_setup_trb() {
    let kind = if cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup) {
        TRB_XBL_EP0_SETUP
    } else {
        TRB_CONTROL_SETUP
    };
    unsafe {
        let setup = ep0_setup_data_ptr();
        if setup != ep0_trb_ptr(0).cast::<u8>() {
            core::ptr::write_bytes(setup, 0, 8);
            cache_clean(setup as usize, 8);
        }
        prepare_trb(0, setup as *const u8, 8, kind);
    }
}

/// Write Memory Barrier for TRB publication, matching the SHIPPED kernel.
///
/// The shipped `__dwc3_prepare_one_trb()` (`gadget.c`) replaced a plain `mb()`
/// with `wmb()` immediately before setting `DWC3_TRB_CTRL_HWO`, with this
/// comment:
///
/// > As per data book 4.2.3.2 TRB Control Bit Rules section: The controller
/// > autonomously checks the HWO field of a TRB to determine if the entire TRB
/// > is valid. Therefore, software must ensure that the rest of the TRB is
/// > valid before setting the HWO field to '1'. ... However there is a
/// > possibility of CPU re-ordering here which can cause controller to observe
/// > the HWO bit set prematurely. Add a write memory barrier to prevent CPU
/// > re-ordering.
///
/// On arm64 Linux `wmb()` is `dsb st`. `write_volatile` orders nothing by
/// itself, and `cache_clean()` is a no-op unless the DT describes a
/// non-coherent path (see `cache_clean`), so without this the core can observe
/// HWO before `bpl`/`bph`/`size` have landed - which is one way the core ends
/// up never consuming the TRB.
#[inline]
unsafe fn trb_publish_barrier() {
    unsafe { core::arch::asm!("dsb st", options(nostack, preserves_flags)) };
}

unsafe fn prepare_trb_at(trb: *mut Trb, buffer: *const u8, length: usize, kind: u32) {
    let address = unsafe { dma_iova_for(buffer as usize) };
    unsafe {
        write_volatile(addr_of_mut!((*trb).bpl), address as u32);
        write_volatile(addr_of_mut!((*trb).bph), (address >> 32) as u32);
        write_volatile(addr_of_mut!((*trb).size), length as u32);
        // The fourth DWORD carries HWO and must be published last.
        trb_publish_barrier();
        write_volatile(
            addr_of_mut!((*trb).ctrl),
            kind | TRB_HWO | TRB_LST | TRB_IOC | TRB_ISP_IMI,
        );
        cache_clean(trb as usize, core::mem::size_of::<Trb>());
    }
}

unsafe fn start_setup() -> bool {
    trace_event(TRACE_SETUP_QUEUED, 0, 0, 0, 8, unsafe { read(DSTS) });
    unsafe {
        if cfg!(any(
            fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup,
            fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0
        )) {
            if !queue_xbl_setup_request() {
                return false;
            }
        }
        // Keep the source-derived TRBCTL=2 and pre-Run/Stop start order. The
        // XBL setup branch also publishes the generated TRB address as its
        // buffer field; `prepare_ep0_setup_trb()` carries that special case.
        prepare_ep0_setup_trb();
        let armed = start_transfer(0, ep0_trb_ptr(0));
        if armed && cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup) {
            let slot = EP0_SETUP_REQUEST_SLOT;
            if slot == usize::MAX || !udc_mut().start(0, slot) {
                return false;
            }
        }
        EP0_SETUP_ARMED = armed;
        armed
    }
}

/// Mirror XBL's bounded software request queue for the deferred EP0 setup.
/// The UDC queue is deliberately separate from the DMA TRB: the event-driven
/// A/B must preserve the queued -> in-flight ownership transition even though
/// the early boot path uses a fixed setup buffer.
unsafe fn queue_xbl_setup_request() -> bool {
    unsafe {
        if !cfg!(any(
            fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup,
            fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0
        )) {
            return true;
        }
        if EP0_SETUP_REQUEST_SLOT != usize::MAX
            && udc_mut().request(0, EP0_SETUP_REQUEST_SLOT).is_some()
        {
            return true;
        }
        EP0_SETUP_REQUEST_SLOT = usize::MAX;
        let Some(slot) = udc_mut().queue(0, 8) else {
            trace_event(TRACE_USB_DEVICE_ERROR, 0x58425155, 0, 0, 8, read(DSTS)); // "XBQU"
            return false;
        };
        EP0_SETUP_REQUEST_SLOT = slot;
        true
    }
}

/// Retire the XBL-style setup request after its setup TRB completes. A stale
/// slot is harmless after a reset because `queue_xbl_setup_request()` checks
/// the UDC object before reusing it.
unsafe fn complete_xbl_setup_request(error: bool) {
    unsafe {
        if !cfg!(any(
            fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup,
            fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0
        )) {
            return;
        }
        let slot = EP0_SETUP_REQUEST_SLOT;
        if slot != usize::MAX {
            let _ = udc_mut().complete(0, slot, if error { 0 } else { 8 }, error);
            let _ = udc_mut().release(0, slot);
            EP0_SETUP_REQUEST_SLOT = usize::MAX;
        }
    }
}

/// Exercise the live EP0 event-DMA path after Run/Stop without changing the
/// host-facing SETUP transfer. The old diagnostic ended the live request and
/// issued a synthetic STARTTRANSFER/ENDTRANSFER pair; on hardware that could
/// remove the only EP0 request before host attach. GETEPSTATE is a read-only
/// endpoint command and, with CMDIOC, still gives us a command-complete event
/// to observe in the same event buffer.
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
unsafe fn post_runstop_event_dma_probe() -> bool {
    unsafe {
        let command_ok = send_ep_command(0, DEPCMD_GETEPSTATE | DEPCMD_CMDIOC, 0, 0, 0);
        let mut delivered = false;
        let mut event_word = 0u32;
        for _ in 0..100 {
            super::timer::delay_ms(1);
            if read(GEVNTCOUNT0) & GEVNTCOUNT_MASK != 0 {
                delivered = true;
                break;
            }
        }
        if delivered {
            let slot = (EVENT_OFFSET % ep0_event_size()) & !0x3;
            event_word = read_volatile((ep0_event_dma_base() + slot) as *const u32);
            poll_ep0_event_ring();
        }
        trace_event(
            TRACE_EVENT_RING_READY,
            0x504F_5354, // "POST"
            command_ok as u32,
            EP0_SETUP_ARMED as u32,
            event_word | ((delivered as u32) << 31),
            read(DSTS),
        );
        if !command_ok || !delivered {
            trace_marker(TRACE_PROBE_WATCHDOG, 0x504F_5346); // "POSF"
            log_puts("usb: post-Run/Stop event DMA probe failed\n");
            return false;
        }
        true
    }
}

/// Best-effort SETUP arming for the poll-loop guard. Unlike `rearm_setup()`
/// this never tears the endpoint down on failure: the core rejects Start
/// Transfer while the link is not ON, and the guard simply retries on the
/// next poll until the link comes up.
unsafe fn try_arm_setup() -> bool {
    unsafe {
        if EP0_SETUP_ARMED || !ENDPOINTS_READY || EP0_STATE != Ep0State::Setup {
            return EP0_SETUP_ARMED;
        }
        if ARM_COOLDOWN != 0 {
            ARM_COOLDOWN -= 1;
            return false;
        }
        // The normal path waits for the controller's U0 report because the
        // DWC3 programming guide rejects Start Transfer in suspend/reset.
        // POST-RECOVERY CORRECTION: the gate stays closed forever on this
        // revision (DSTS reads non-U0 link states 4/6/7/10/12/13 while the
        // host is already issuing tokens), which blocked every SETUP re-arm
        // and NAKed the whole first descriptor window (-110). The bus reset
        // ends before the host's first SETUP token, so issuing the re-arm
        // here is safe; only a genuinely halted controller is skipped.
        #[cfg(not(fullerene_aarch64_usb_gadget_handoff_start_ungated))]
        {
            let dsts = read(DSTS);
            if dsts & DSTS_DEVCTRLHLT != 0 {
                SETUP_ARM_FAILURE_STAGE = 1;
                return false;
            }
        }
        // This A/B deliberately issues STARTTRANSFER without consulting the
        // firmware-inherited DSTS halt/link readback. The Bramble handoff can
        // report a stale HALT bit while the host has already reached the
        // attach boundary; let the command result, rather than that status
        // read, decide whether the EP0 SETUP TRB can be armed.
        prepare_ep0_setup_trb();
        if start_transfer(0, ep0_trb_ptr(0)) {
            SETUP_ARM_FAILURE_STAGE = 0;
            EP0_SETUP_ARMED = true;
            PENDING_SETUP_ARM = false;
            trace_event(TRACE_SETUP_QUEUED, 0x4152_4D45, 0, 0, 0, read(DSTS)); // "ARME"
            true
        } else {
            #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_retry_setup)]
            if retry_start_transfer(0, ep0_trb_ptr(0), 400) {
                SETUP_ARM_FAILURE_STAGE = 0;
                EP0_SETUP_ARMED = true;
                PENDING_SETUP_ARM = false;
                trace_event(TRACE_SETUP_QUEUED, 0x5254_5259, 0, 0, 0, read(DSTS)); // "RTRY"
                return true;
            }
            // Fast-fail ("No resource" completes immediately). The host's
            // first SETUP token lands ~1 ms after its bus reset ends, so the
            // retry rate must place an armed SETUP TRB inside that window
            // while still bounding the total failed-command count.
            // BISECT: the 20-poll cooldown variant never reached the data
            // phase (-110 in every run); the 200-poll cooldown is the value
            // from the era when the device answered with a data-phase -71.
            SETUP_ARM_FAILURE_STAGE = if SETUP_ARM_LAST_COMMAND & 0x8000_0000 != 0
                || SETUP_ARM_LAST_COMMAND == 0xffff_ffff
            {
                3
            } else {
                2
            };
            ARM_COOLDOWN = 200;
            false
        }
    }
}

/// Recover an EP0 completion when the DWC3 event FIFO is empty but the
/// controller has already retired the DMA TRB.
///
/// This is deliberately opt-in and read-first: it never acknowledges
/// GEVNTCOUNT and never submits a command on its own.  A missing event-ring
/// record must not prevent a host SETUP/DATA/STATUS transfer from progressing
/// when the endpoint DMA engine did complete it.  The synthetic event enters
/// the same `process_event()` path as a normal DEPEVT_XferComplete, so the
/// existing gadget state machine remains the single owner of EP0 transitions.
unsafe fn poll_ep0_trb_completion_fallback() -> bool {
    if !cfg!(fullerene_aarch64_usb_gadget_handoff_ep0_trb_completion_fallback) {
        return false;
    }

    unsafe {
        let endpoint = match EP0_STATE {
            Ep0State::Setup => 0,
            Ep0State::Data => usize::from(CONTROL_IN),
            Ep0State::Status => {
                if CONTROL_HAS_DATA && CONTROL_IN {
                    0
                } else {
                    1
                }
            }
        };
        let trb = ep0_trb_ptr(ep0_trb_index(endpoint));
        cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
        let ctrl = read_volatile(addr_of!((*trb).ctrl));

        // A SETUP payload is actionable even if this revision failed to clear
        // HWO while delivering the packet.  The setup buffer is aliased to
        // TRB0 on the source-aligned path, so compare its first two words with
        // the expected DMA address instead of treating the normal TRB address
        // as a stale packet.  DATA/STATUS still require a retired TRB because
        // their response buffer has no equivalent packet marker.
        let mut setup_received = false;
        if EP0_STATE == Ep0State::Setup {
            let setup = ep0_setup_data_ptr();
            cache_invalidate(setup as usize, 8);
            if setup == trb.cast::<u8>() {
                let expected = dma_iova_for(setup as usize);
                let current_low = read_volatile(setup.cast::<u32>());
                let current_high = read_volatile(setup.add(4).cast::<u32>());
                setup_received =
                    current_low != expected as u32 || current_high != (expected >> 32) as u32;
            } else {
                for offset in 0..8 {
                    if read_volatile(setup.add(offset)) != 0 {
                        setup_received = true;
                        break;
                    }
                }
            }
        }
        if (EP0_STATE == Ep0State::Setup && !setup_received)
            || (EP0_STATE != Ep0State::Setup && ctrl & TRB_HWO != 0)
        {
            return false;
        }

        // DEPEVT_XferComplete: endpoint[5:1], event[9:6] = 1, and the
        // normal LST completion status in bits[15:12].
        let raw = ((endpoint as u32) << 1) | (1 << 6) | (0x8 << 12);
        trace_event(
            TRACE_TRANSFER_COMPLETE,
            0x444D_4150, // "DMAP": completion inferred from retired TRB
            endpoint as u32,
            ctrl,
            read(GEVNTCOUNT0),
            read(DSTS),
        );
        process_event(raw);
        true
    }
}

/// Re-arm EP0 only after a successful STARTTRANSFER command. On failure the
/// endpoint is removed from DALEPENA so a host cannot continue sending SETUP
/// packets into a stale resource; the next Connect Done/USB reset can rebuild
/// the endpoint allocation.
unsafe fn rearm_setup() -> bool {
    // A failed Start Transfer on this core means the device link is not ON
    // yet (the host's bus reset is in flight) - never a broken endpoint. The
    // old punitive path (DALEPENA clear + ENDPOINTS_READY=false) killed EP0
    // exactly when the host's post-reset descriptor read arrived, which is
    // the source of the first-read -110. Leave the endpoint alive: the
    // poll-loop guard retries the arm the moment the link reaches ON.
    if unsafe { start_setup() } {
        return true;
    }
    unsafe {
        trace_event(TRACE_USB_DEVICE_ERROR, 0, 0, 0, 0, read(DSTS));
    }
    false
}

/// Tear down every opt-in GSI channel before a USB reset or Type-C detach.
/// Linux removes queued gadget requests before reusing the endpoint; merely
/// clearing the bookkeeping here would leave DWC3 owning stale TRBs and an
/// outstanding resource index.
unsafe fn reset_gsi_channels() {
    unsafe {
        for index in 0..3 {
            let endpoint = GSI_CHANNEL_ENDPOINT[index];
            let event_buffer = (index + 1) as u32;
            let endpoint_address = endpoint as u8 | if endpoint & 1 != 0 { 0x80 } else { 0 };
            let request_slot = GSI_REQUEST_SLOTS[index];
            if GSI_RING_ACTIVE[index] && endpoint >= 2 && GSI_RESOURCE_INDEX[index] != 0 {
                let _ = end_gsi_transfer(endpoint, event_buffer);
            }
            if request_slot != usize::MAX {
                // ENDTRANSFER must revoke DWC3 ownership before the gadget
                // request slot is returned to the function layer.
                let _ = udc_mut().release(endpoint_address, request_slot);
            }
            if GSI_CHANNEL_READY[index] && endpoint >= 2 {
                let _ = udc_mut().disable_endpoint(endpoint_address);
                write(DALEPENA, read(DALEPENA) & !(1 << endpoint));
            }
            GSI_PENDING[index] = false;
            GSI_REQUEST_SLOTS[index] = usize::MAX;
            GSI_RING_ACTIVE[index] = false;
            GSI_RESOURCE_INDEX[index] = 0;
            GSI_RING_BASES[index] = 0;
            GSI_RING_TRB_COUNTS[index] = 0;
            GSI_BUFFER_BASES[index] = 0;
            GSI_BUFFER_LENGTHS[index] = 0;
            GSI_DOORBELL_BASES[index] = 0;
            GSI_CHANNEL_READY[index] = false;
            GSI_CHANNEL_ENDPOINT[index] = 0;
        }
        GSI_GADGET_BOUND = false;
    }
}

/// Re-arm the control endpoint after the host has issued a USB bus reset.
///
/// A bus reset terminates the setup transfer which was queued before the
/// host began enumeration, but it does not perform a DWC3 core reset.  The
/// transfer resources and endpoint configuration therefore remain usable.
/// Keeping DALEPENA cleared here leaves the device with a pull-up and no EP0,
/// which is indistinguishable from a dead gadget to the host.
unsafe fn restart_control_after_reset() {
    unsafe {
        // qpr1 leaves GCTL.RAMCLKSEL at the reset value after the host's bus
        // USB reset; reapply_ramclksel() is a disabled-by-default legacy A/B.
        reapply_ramclksel();
        // Linux's dwc3_ep0_reset_state() is a NO-OP while EP0 sits in the
        // SETUP phase: the armed SETUP TRB stays valid across a USB reset,
        // and the reset handler must not tear it down or re-arm. Rewriting
        // DALEPENA, reprogramming DCFG.speed, or issuing a second Start
        // Transfer all race the host's first SETUP token (which lands ~1 ms
        // after the reset ends) and are the source of the first descriptor
        // read/64 error -110. When the TRB is armed, only clear the device
        // address (the hardware already did; Linux rewrites it) and reset the
        // software state that does not touch the armed transfer.
        if EP0_STATE == Ep0State::Setup && EP0_SETUP_ARMED && ENDPOINTS_READY {
            #[cfg(fullerene_aarch64_usb_gadget_handoff_ep0_reset_android_state_order)]
            {
                // qpr1's dwc3_gadget_reset_interrupt() notifies the gadget,
                // clears test mode, revokes active transfers, and clears
                // endpoint stalls in that order. The ordinary Fullerene path
                // intentionally preserves its armed control transfer; this
                // source-order A/B follows qpr1 and lets the common re-arm
                // tail below publish a fresh SETUP transfer.
                GadgetDriver::reset(gadget_mut());
                let dctl = read(DCTL);
                write(DCTL, dctl & !DCTL_TSTCTRL_MASK);
                let transfer_ok = stop_active_ep0_at_reset();
                let out_ok = send_ep_command(0, DEPCMD_CLEARSTALL, 0, 0, 0);
                let in_ok = send_ep_command(1, DEPCMD_CLEARSTALL, 0, 0, 0);
                EP0_SETUP_ARMED = false;
                trace_event(
                    TRACE_USB_RESET,
                    0x51525354, // "QRST": qpr1 reset sequence
                    transfer_ok as u32,
                    (out_ok as u32) | ((in_ok as u32) << 1),
                    dctl & DCTL_TSTCTRL_MASK,
                    read(DSTS),
                );
            }
            if !cfg!(fullerene_aarch64_usb_gadget_handoff_ep0_reset_android_state_order) {
                #[cfg(fullerene_aarch64_usb_gadget_handoff_ep0_reset_android_state_order)]
                {
                    // Android msm's dwc3_gadget_reset_interrupt() first notifies
                    // the gadget driver, then clears DCTL.TSTCTRL, and only then
                    // clears endpoint stalls. Keep the armed EP0 SETUP transfer
                    // and DMA addresses intact while reproducing that complete
                    // reset-time state order as one explicit hardware A/B.
                    GadgetDriver::reset(gadget_mut());
                    let dctl = read(DCTL);
                    write(DCTL, dctl & !DCTL_TSTCTRL_MASK);
                    let out_ok = send_ep_command(0, DEPCMD_CLEARSTALL, 0, 0, 0);
                    let in_ok = send_ep_command(1, DEPCMD_CLEARSTALL, 0, 0, 0);
                    trace_event(
                        TRACE_USB_RESET,
                        0x41525354, // "ARST"
                        out_ok as u32,
                        in_ok as u32,
                        dctl & DCTL_TSTCTRL_MASK,
                        read(DSTS),
                    );
                }
                #[cfg(all(
                    fullerene_aarch64_usb_gadget_handoff_ep0_reset_callback_first,
                    not(fullerene_aarch64_usb_gadget_handoff_ep0_reset_android_state_order)
                ))]
                {
                    // Android msm calls usb_gadget_udc_reset() before its
                    // controller-side stop/clear-stall cleanup. Move only the
                    // existing gadget callback in this A/B; EP0 ownership and
                    // all controller commands remain otherwise unchanged.
                    GadgetDriver::reset(gadget_mut());
                    trace_event(
                        TRACE_USB_RESET,
                        0x52434246, // "RCBF"
                        1,
                        0,
                        0,
                        read(DSTS),
                    );
                }
                #[cfg(all(
                    fullerene_aarch64_usb_gadget_handoff_ep0_reset_clear_stall,
                    not(fullerene_aarch64_usb_gadget_handoff_ep0_reset_android_state_order)
                ))]
                {
                    // Android msm's dwc3_clear_stall_all_ep() clears EP0 OUT and
                    // IN after USB Reset without stopping or re-arming the
                    // preserved SETUP transfer. Keep this as an isolated A/B:
                    // the normal preserve path must not issue extra commands.
                    let out_ok = send_ep_command(0, DEPCMD_CLEARSTALL, 0, 0, 0);
                    let in_ok = send_ep_command(1, DEPCMD_CLEARSTALL, 0, 0, 0);
                    trace_event(
                        TRACE_USB_RESET,
                        0x5253544C, // "RSTL"
                        out_ok as u32,
                        in_ok as u32,
                        0,
                        read(DSTS),
                    );
                }
                #[cfg(all(
                    fullerene_aarch64_usb_gadget_handoff_ep0_reset_clear_test_mode,
                    not(fullerene_aarch64_usb_gadget_handoff_ep0_reset_android_state_order)
                ))]
                {
                    // Android msm clears DCTL.TSTCTRL in its bus-reset handler
                    // before preserving the EP0 SETUP transfer. Apply only that
                    // register correction in this A/B; Run/Stop and the EP0
                    // ownership boundary remain unchanged.
                    let dctl = read(DCTL);
                    write(DCTL, dctl & !DCTL_TSTCTRL_MASK);
                    trace_event(
                        TRACE_USB_RESET,
                        0x54455354, // "TEST"
                        dctl & DCTL_TSTCTRL_MASK,
                        0,
                        0,
                        read(DSTS),
                    );
                }
                if cfg!(fullerene_aarch64_usb_gadget_handoff_reset_resource) {
                    // This opt-in A/B deliberately tests the opposite hardware
                    // hypothesis from the Android-compatible preserve path: a
                    // bus reset may leave the EP0 contexts intact while losing
                    // their transfer-resource allocation. Re-issue only
                    // SETTRANSFRESOURCE and keep the armed SETUP TRB, endpoint
                    // ownership, and returned STARTTRANSFER indices unchanged.
                    let out_ok = set_transfer_resource(0);
                    let in_ok = set_transfer_resource(1);
                    trace_event(
                        TRACE_USB_RESET,
                        0x52535243, // "RSRC"
                        out_ok as u32,
                        in_ok as u32,
                        1,
                        read(DSTS),
                    );
                }
                let dcfg = read(DCFG) & !DCFG_DEVADDR_MASK;
                write(DCFG, dcfg);
                unbind_function();
                teardown_data_endpoints();
                reset_gsi_channels();
                #[cfg(not(any(
                    fullerene_aarch64_usb_gadget_handoff_ep0_reset_callback_first,
                    fullerene_aarch64_usb_gadget_handoff_ep0_reset_android_state_order
                )))]
                GadgetDriver::reset(gadget_mut());
                udc_mut().reset();
                CONFIGURED = false;
                DATA_ENDPOINTS_READY = false;
                DATA_REQUEST_SLOTS = [usize::MAX; 2];
                DATA_RESOURCE_INDEX = [0; 2];
                GSI_GADGET_BOUND = false;
                FUNCTION_BOUND = false;
                CONTROL_IN = false;
                CONTROL_HAS_DATA = false;
                if cfg!(fullerene_aarch64_usb_gadget_handoff_reset_endpoints) {
                    // This broader opt-in A/B tests whether the endpoint context
                    // itself is lost across the bus reset. Unlike the normal
                    // Android-compatible preserve path, rebuild both EP0
                    // contexts, clear the old STARTTRANSFER resource indices,
                    // and let the ordinary post-reset arm retry at link ON.
                    let speed = read(DSTS) & DSTS_CONNECTSPD_MASK;
                    let max_packet = if speed == DSTS_SUPERSPEED { 512 } else { 64 };
                    write(DALEPENA, 0);
                    let rebuilt = send_ep_command(0, DEPCMD_DEPSTARTCFG, 0, 0, 0)
                        && configure_endpoint(0, max_packet, false)
                        && configure_endpoint(1, max_packet, false);
                    if rebuilt {
                        let _ = udc_mut().configure_endpoint(0, max_packet as u16, false);
                        let _ = udc_mut().configure_endpoint(1, max_packet as u16, false);
                    } else {
                        log_puts("usb: EP0 reset endpoint rebuild failed\n");
                    }
                    ENDPOINTS_READY = rebuilt;
                    EP0_RESOURCE_INDEX = [0; 2];
                    EP0_SETUP_ARMED = false;
                    PENDING_SETUP_ARM = true;
                    write(DALEPENA, if rebuilt { 0b11 } else { 0 });
                    trace_event(
                        TRACE_USB_RESET,
                        0x52455043, // "REPC"
                        rebuilt as u32,
                        max_packet,
                        0,
                        read(DSTS),
                    );
                    let _ = try_arm_setup();
                }
                // In the default path EP0_STATE, EP0_SETUP_ARMED,
                // EP0_RESOURCE_INDEX, ENDPOINTS_READY, DALEPENA, DCFG.speed, and
                // the armed SETUP TRB are preserved. Android msm's
                // dwc3_gadget_reset_interrupt() does not stop or re-arm EP0: the
                // initial SETUP transfer remains owned by the core across USB
                // reset, and Connect Done later MODIFYs the EP0 contexts for the
                // negotiated speed. Issuing ENDTRANSFER or a second STARTTRANSFER
                // here races the host's first post-reset SETUP token and loses
                // the descriptor window.
                trace_event(
                    TRACE_USB_RESET,
                    0x4B45_504B, // "KEEP"
                    0,
                    0,
                    0,
                    read(DSTS),
                );
                return;
            }
        }
        // A bus reset already flushed every in-flight EP0 transfer at the
        // wire level. Issuing ENDXFER here and then re-arming races the
        // resource release against the new Start Transfer: the core answers
        // the re-arm with "No Resource" until the ENDXFER completes, the
        // re-arm lands after the host's post-reset SETUP token, and the
        // first descriptor read times out (-110). Clear only the software
        // index; the hardware transfer state is reset by the bus reset.
        EP0_RESOURCE_INDEX = [0; 2];
        unbind_function();
        teardown_data_endpoints();
        reset_gsi_channels();
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_ep0_reset_android_state_order) {
            GadgetDriver::reset(gadget_mut());
        }
        udc_mut().reset();
        CONFIGURED = false;
        DATA_ENDPOINTS_READY = false;
        DATA_REQUEST_SLOTS = [usize::MAX; 2];
        DATA_RESOURCE_INDEX = [0; 2];
        // A USB bus reset terminates the active DWC3 EP0 transfer. Linux
        // drops the cached resource index at this boundary; retaining it
        // can make the next STARTTRANSFER look like a continuation of the
        // old Fastboot/control session on some DWC3 revisions.
        EP0_RESOURCE_INDEX = [0; 2];
        EP0_SETUP_ARMED = false;
        PENDING_SETUP_ARM = true;
        GSI_GADGET_BOUND = false;
        FUNCTION_BOUND = false;
        EP0_STATE = Ep0State::Setup;
        DATA_PHASE_PENDING_START = false;
        CONTROL_IN = false;
        CONTROL_HAS_DATA = false;

        let mut dcfg = read(DCFG) & !DCFG_DEVADDR_MASK;
        let speed = read(DSTS) & DSTS_CONNECTSPD_MASK;
        let max_packet = if speed == DSTS_SUPERSPEED { 512 } else { 64 };
        dcfg &= !DCFG_SPEED_MASK;
        dcfg |= if speed == DSTS_SUPERSPEED {
            DCFG_SUPERSPEED
        } else {
            DCFG_HIGHSPEED
        };
        write(DCFG, dcfg);

        // USB reset ends the active EP0 transfer, but the endpoint remains
        // configured on the non-core-reset path.  Reconfigure defensively
        // if a preceding Connect Done event did not get processed.
        if !ENDPOINTS_READY {
            ENDPOINTS_READY = configure_endpoint(0, max_packet, false)
                && configure_endpoint(1, max_packet, false);
        }
        if ENDPOINTS_READY {
            let _ = udc_mut().configure_endpoint(0, max_packet as u16, false);
            let _ = udc_mut().configure_endpoint(1, max_packet as u16, false);
            // Some handoff revisions flush the EP0 allocation window at USB
            // bus reset even though the controller remains in device mode.
            // Rebuild the endpoint state only for this A/B so the normal
            // reset path keeps the Linux-compatible behavior.
            if cfg!(fullerene_aarch64_usb_gadget_handoff_reset_endpoints) {
                write(DALEPENA, 0);
                let rebuilt = send_ep_command(0, DEPCMD_DEPSTARTCFG, 0, 0, 0)
                    && configure_endpoint(0, max_packet, false)
                    && configure_endpoint(1, max_packet, false);
                if !rebuilt {
                    log_puts("usb: EP0 reset endpoint rebuild failed\n");
                    trace_event(TRACE_USB_RESET, 0x52455043, 0, 0, 0, read(DSTS));
                }
                ENDPOINTS_READY = rebuilt;
            } else if cfg!(fullerene_aarch64_usb_gadget_handoff_reset_resource) {
                // A narrower resource-only A/B for revisions where the
                // endpoint configuration survives but the allocation does
                // not.
                let out_ready = set_transfer_resource(0);
                let in_ready = set_transfer_resource(1);
                if !out_ready || !in_ready {
                    log_puts("usb: EP0 reset resource reallocation failed\n");
                    trace_event(
                        TRACE_USB_RESET,
                        0x52535243, // "RSRC"
                        out_ready as u32,
                        in_ready as u32,
                        0,
                        read(DSTS),
                    );
                }
            }
            write(DALEPENA, 0b11);
            if cfg!(any(
                fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup,
                fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0
            )) {
                // USB reset cleared the software request object; the
                // historical XBL differential posts its 8-byte EP0 OUT
                // request again. This is not the canonical initial SETUP
                // arm boundary: Linux/Android arm CONTROL_SETUP eagerly.
                let _ = queue_xbl_setup_request();
            }
            // The host's bus reset is still in progress when this event is
            // processed, and the core rejects Start Transfer until the link
            // returns to ON. Use the non-punitive arm: a failure here just
            // leaves the arming to the poll-loop guard, which fires the
            // moment the link is up and delivers any latched SETUP.
            let _ = try_arm_setup();
        }
    }
}

/// Reflect gadget-core state into the two pieces of DWC3 device state that
/// are committed only after a successful control status stage.  Linux does
/// not apply SET_ADDRESS or SET_CONFIGURATION at SETUP reception time.
unsafe fn sync_gadget_state() {
    unsafe {
        let address = gadget_ref().address() as u32;
        let dcfg = read(DCFG) & !DCFG_DEVADDR_MASK;
        write(DCFG, dcfg | (address << 3));
        CONFIGURED = gadget_ref().configured();
        udc_mut().address = gadget_ref().address();
        udc_mut().configured = CONFIGURED;
        if CONFIGURED && !DATA_ENDPOINTS_READY {
            // The protocol layer exposes one ADB-class-compatible function
            // with either an ordinary bulk pair or an explicitly supplied
            // IPA/GSI binding. Configure it only after SET_CONFIGURATION has
            // committed, matching gadget-core ordering.
            let gsi_config = gadget_ref().gsi_endpoint();
            if let Some(config) = gsi_config {
                if let Some((ring, buffers)) = configure_gsi_data_endpoint(
                    config.endpoint,
                    config.event_buffer,
                    config.max_packet,
                    config.doorbell,
                    config.buffer_length,
                ) {
                    GSI_GADGET_BOUND = true;
                    gadget_mut().on_gsi_channel_ready(config, ring, buffers);
                }
            }

            if !GSI_GADGET_BOUND {
                // Linux calls dwc3_gadget_start_config(2) when
                // SET_CONFIGURATION commits. DEPSTARTCFG(2) resets only
                // non-control endpoint resource allocation; omitting this
                // boundary leaves EP2/EP3 in Fastboot's allocation epoch and
                // can tear down the link immediately after enumeration.
                let data_ready =
                    send_ep_command(0, DEPCMD_DEPSTARTCFG | (2 << DEPCMD_PARAM_SHIFT), 0, 0, 0)
                        && configure_endpoint_kind(2, 512, DEPCFG_EP_TYPE_BULK, false)
                        && configure_endpoint_kind(3, 512, DEPCFG_EP_TYPE_BULK, false);
                if data_ready
                    && udc_mut().configure_endpoint(0x02, 512, true)
                    && udc_mut().configure_endpoint(0x83, 512, true)
                {
                    write(DALEPENA, read(DALEPENA) | (1 << 2) | (1 << 3));
                    DATA_ENDPOINTS_READY = true;
                    // Bind the function only after SET_CONFIGURATION has
                    // committed. Queueing the OUT request here makes the
                    // ordinary UDC data path live before the first packet.
                    FUNCTION_BOUND = true;
                    GadgetDriver::on_function_bind(gadget_mut());
                    let _ = queue_bulk_transfer(
                        2,
                        addr_of_mut!(DATA_OUT_BUFFER.0).cast::<u8>(),
                        DATA_OUT_BUFFER_SIZE,
                    );
                }
            } else {
                FUNCTION_BOUND = true;
                GadgetDriver::on_function_bind(gadget_mut());
            }
        } else if !CONFIGURED && (DATA_ENDPOINTS_READY || GSI_GADGET_BOUND) {
            teardown_data_endpoints();
            if GSI_GADGET_BOUND {
                reset_gsi_channels();
            }
            unbind_function();
        }
    }
}

unsafe fn start_status(endpoint: usize) -> bool {
    let kind = if unsafe { CONTROL_HAS_DATA } {
        TRB_CONTROL_STATUS3
    } else {
        TRB_CONTROL_STATUS2
    };
    trace_event(TRACE_STATUS_QUEUED, 0, endpoint as u32, kind, 0, unsafe {
        read(DSTS)
    });
    unsafe {
        let trb_index = ep0_trb_index(endpoint);
        prepare_trb(trb_index, ep0_trb_ptr(trb_index).cast::<u8>(), 0, kind);
        // Same flaky Start Transfer window as the data phase: retry the
        // command instead of failing the status stage (SET_ADDRESS and
        // SET_CONFIGURATION become visible only after this ZLP completes).
        retry_start_transfer(endpoint, ep0_trb_ptr(trb_index), 2500)
    }
}

unsafe fn stall_control(endpoint: usize) {
    // Linux's gadget core responds to an unsupported control request with a
    // real EP0 STALL. Leaving the endpoint idle is not equivalent: hosts may
    // keep waiting for the missing handshake and never issue the next SETUP.
    let _ = unsafe { send_ep_command(endpoint, DEPCMD_SETSTALL, 0, 0, 0) };
    unsafe {
        EP0_STATE = Ep0State::Setup;
        DATA_PHASE_PENDING_START = false;
    }
}

/// Answer a host SETUP that arrived without a device event.
///
/// `handle_setup()` already latches the packet out of the EP0 SETUP DMA buffer
/// and zeroes that buffer, so a non-zero buffer is a fresh packet. The event
/// path is the normal trigger, but on this handoff `GEVNTCOUNT0` stays 0 even
/// though the host's traffic reaches the controller, so polling the buffer is
/// the only way left to answer the host's `GET_DESCRIPTOR`. Selected with
/// `--utmi-postrun-readout usb2-live-setup-poll`; it is a no-op otherwise.
unsafe fn poll_setup_buffer() -> bool {
    // `usb2-live-force-endpoints` and `usb2-live-rearm-loop` bundle this path:
    // those selectors exist to answer the host with no device events at all,
    // which is exactly when this eventless SETUP reader is needed.
    // NO GATE: answering the host must not depend on a diagnostic selector being passed.
    // In every ordinary run this returned false immediately, `handle_setup()` never ran, and
    // the host's GET_DESCRIPTOR went unanswered. See usb/README.md §2 (defect 6).
    // Cost when idle: one `cache_invalidate` and eight `read_volatile`s per pass, and
    // `handle_setup()` only when the buffer is non-zero.
    unsafe {
    // POLL-SETUP-BUFFER BREADCRUMBS (level 9). `bc3!(1)` fires and `bc3!(2)` does not, with only
    // this call between them, and `bc8!(8)` shows `handle_setup()` was never entered - so the block
    // is one of the three DRAM-side steps below. None of them is MMIO, so whichever it is needs a
    // specific explanation (cache maintenance on a bad address, or a read that resolves through a
    // mapping that never completes). See usb/README.md §3.27.
    macro_rules! bc9 {
        ($i:expr) => {
            if matches!(
                option_env!("FULLERENE_USB_PULSE_BREADCRUMB"),
                Some("9") | Some("10") | Some("4")
            ) && unsafe { !trace::BC_ONCE[$i] }
            {
                unsafe {
                    trace::BC_ONCE[$i] = true;
                    ccs_pulse(300);
                }
            }
        };
    }
    bc9!(16); // poll_setup_buffer entered
        let setup = ep0_setup_data_ptr();
        bc9!(17); // ep0_setup_data_ptr() returned
        cache_invalidate(setup as usize, 8);
        bc9!(18); // cache_invalidate returned
        let mut fresh = false;
        for offset in 0..8 {
            if read_volatile(setup.add(offset)) != 0 {
                fresh = true;
                break;
            }
        }
        bc9!(19); // the eight-byte DRAM probe returned
        if fresh {
            trace_marker(TRACE_SETUP_RECEIVED, 0x5345_5450); // "SETP"
            handle_setup();
        }
        fresh
    }
}

unsafe fn setup_request() -> [u8; 8] {
    let mut packet = [0; 8];
    unsafe {
        let setup = ep0_setup_data_ptr();
        cache_invalidate(setup as usize, 8);
        core::ptr::copy_nonoverlapping(setup, packet.as_mut_ptr(), 8);
    }
    packet
}

unsafe fn handle_setup() {
    let packet = unsafe { setup_request() };
    // HANDLE-SETUP BREADCRUMBS (level 8). `poll_setup_buffer()` is DRAM-only and cannot block, yet
    // it never returns after `bc3!(1)`. That leaves `handle_setup()` - reached only when the SETUP
    // buffer holds non-zero bytes, which means the host's SETUP really did arrive - and its last
    // two stages touch the controller. These four pulses say which stage does not return.
    // See usb/README.md §3.26.
    macro_rules! bc8 {
        ($i:expr) => {
            if matches!(
                option_env!("FULLERENE_USB_PULSE_BREADCRUMB"),
                Some("8") | Some("4")
            ) && unsafe { !trace::BC_ONCE[$i] }
            {
                unsafe {
                    trace::BC_ONCE[$i] = true;
                    ccs_pulse(300);
                }
            }
        };
    }
    bc8!(8); // handle_setup() entered
    #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
    unsafe {
        // The SETUP buffer is cleared immediately below so a later poll can
        // distinguish a fresh packet. Preserve a diagnostic latch before
        // clearing it; otherwise the host-visible early-drop probe can miss
        // a real DMA delivery between two 1 ms samples.
        SIGNAL_SETUP_PACKET_RECEIVED = true;
    }
    bc8!(9); // setup_request() + the DRAM-only packet parse returned
    // Zero the DMA buffer after latching the packet: a later non-zero
    // buffer then proves the core delivered a NEW SETUP packet, even while
    // the software state machine was still in the Data/Status phase (the
    // host aborts in-flight control transfers with a new SETUP - Linux
    // handles this via its setup_packet_pending logic).
    unsafe {
        let setup = ep0_setup_data_ptr();
        core::ptr::write_bytes(setup, 0, 8);
        cache_clean(setup as usize, 8);
    }
    let request_type = packet[0];
    let request = packet[1];
    let value = u16::from_le_bytes([packet[2], packet[3]]);
    let index = u16::from_le_bytes([packet[4], packet[5]]);
    let requested_length = u16::from_le_bytes([packet[6], packet[7]]) as usize;
    let direction_in = request_type & 0x80 != 0;
    trace_event(
        TRACE_SETUP_RECEIVED,
        request as u32,
        value as u32,
        index as u32,
        requested_length as u32,
        unsafe { read(DSTS) },
    );
    unsafe {
    bc8!(10); // trace_event(..., read(DSTS)) returned
        // Record the Connect Done -> first SETUP delay (seconds) so the
        // harvest gates can tell whether the control pipeline ran inside the
        // host's enumeration window or long after the host gave up.
        if TRACE_HARVEST_SETUP_DELAY == 0xFFFF && CONNECT_TICK != 0 {
            let frequency = arch_counter_frequency();
            if frequency != 0 {
                let delta_ticks = arch_counter().saturating_sub(CONNECT_TICK);
                TRACE_HARVEST_SETUP_DELAY = (delta_ticks / frequency).min(0xFFFE) as u32;
            }
        }
        CONTROL_IN = direction_in;
        CONTROL_HAS_DATA = requested_length != 0;
    }

    let action = unsafe {
        let response = core::slice::from_raw_parts_mut(ep0_response_ptr(), 512);
        if request_type == TRACE_CONTROL_REQUEST_TYPE && request == TRACE_CONTROL_REQUEST {
            // Keep trace reads outside the gadget function callback: this is
            // a diagnostic transport over the same EP0 path and must not
            // alter address/configuration state.
            fill_trace_control_response(response, requested_length, value)
                .map(ControlAction::DataIn)
                .unwrap_or(ControlAction::Stall)
        } else {
            // Keep the trace transport in the ordinary EP0 path: a host request
            // for string descriptor 3 can observe the retained cursor even when
            // no UART cable is attached.
            gadget_mut().set_trace_status(trace_head(), trace_last_event());
            GadgetDriver::on_setup(gadget_mut(), packet, response)
        }
    };
    // Latch which action the callback returned: the response-phase words all read their
    bc8!(11); // GadgetDriver::on_setup() returned
    // initial values when no reply is built, so this is the one remaining unknown.
    // See usb/README.md §3.7.
    unsafe {
        LAST_CONTROL_ACTION = match action {
            ControlAction::DataIn(_) => 1,
            ControlAction::StatusIn => 2,
            ControlAction::StatusOut => 3,
            ControlAction::SetHalt(_) => 4,
            ControlAction::ClearHalt(_) => 5,
            ControlAction::Setup => 6,
            ControlAction::Stall => 7,
        };
    }
    match action {
        ControlAction::DataIn(mut length) => unsafe {
            #[cfg(fullerene_aarch64_usb_gadget_handoff_ep0_short_first_desc)]
            if request == 0x06 && (value >> 8) == 0x01 && requested_length == 64 {
                // The first device-descriptor read tolerates a short packet;
                // cap the data phase at 8 bytes so a long-packet TX failure
                // cannot hide behind the host's 5 s control timeout.
                length = length.min(8);
            }
            let response = ep0_response_ptr();
            cache_clean(response as usize, length);
            let data_trb_kind = if cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_ep0_in_data) {
                // Stock XBL's EP0 IN request builder emits a NORMAL TRB
                // (TRBCTL=1), even though this is the control-transfer data
                // phase. Keep this as a narrow A/B; setup and status remain
                // on the ordinary control TRB path.
                TRB_NORMAL
            } else {
                TRB_CONTROL_DATA
            };
            let trb_index = ep0_trb_index(1);
            prepare_trb(trb_index, response, length, data_trb_kind);
            trace_event(
                TRACE_DESCRIPTOR_QUEUED,
                request as u32,
                value as u32,
                index as u32,
                length as u32,
                read(DSTS),
            );
            EP0_STATE = Ep0State::Data;
            // Android msm 4.19's __dwc3_gadget_ep0_queue() starts the DATA
            // phase immediately after preparing the response TRB. Waiting
            // for XferNotReady(DATA) here can miss the host's first IN
            // token and leave EP1 NAKing the entire control window (-110).
            // Keep the NRDY path only as recovery if this initial command is
            // rejected while the link is still settling.
            DATA_PHASE_PENDING_LEN = length;
            let started = retry_start_transfer(1, ep0_trb_ptr(trb_index), 2500);
            DATA_PHASE_PENDING_START = !started;
            if started {
                note_probe_ep0_progress();
            }
        },
        ControlAction::StatusIn => unsafe {
            EP0_STATE = Ep0State::Status;
            // The status phase suffers the same XferNotReady boundary as the
            // data phase: an immediate start_status(1) retires cleanly and is
            // ignored. The XferNotReady(CONTROL_STATUS) handler below owns
            // the arm, with the correct endpoint for the transfer direction.
        },
        ControlAction::Stall => {
            log_puts("usb: unsupported control request\n");
            unsafe { stall_control(if direction_in { 1 } else { 0 }) };
        }
        ControlAction::Setup
        | ControlAction::StatusOut
        | ControlAction::SetHalt(_)
        | ControlAction::ClearHalt(_) => {
            log_puts("usb: invalid gadget control action\n");
            unsafe { stall_control(if direction_in { 1 } else { 0 }) };
        }
    }
}

unsafe fn process_event(raw: u32) {
    EVENTS_CONSUMED = EVENTS_CONSUMED.saturating_add(1);
    let endpoint_event = (raw & 1) == 0;
    if !endpoint_event {
        // DWC3's device event layout is: one_bit[0], device_event[1:7],
        // type[8:11].  The device_event field is zero for ordinary device
        // events; type carries Disconnect, USB Reset, and Connect Done.
        let device_event = (raw >> DEVICE_EVENT_KIND_SHIFT) & DEVICE_EVENT_KIND_MASK;
        match device_event {
            0 => {
                // Disconnect invalidates the active control transfer and the
                // device address. Do not rearm until Connect Done establishes
                // a fresh link, exactly as the Linux gadget lifecycle does.
                DISCONNECT_SEEN = true;
                if DISCONNECT_RAW == 0 {
                    DISCONNECT_RAW = raw;
                }
                unsafe {
                    for endpoint in 0..2 {
                        if EP0_RESOURCE_INDEX[endpoint] != 0 {
                            let _ = end_transfer(endpoint);
                            EP0_RESOURCE_INDEX[endpoint] = 0;
                        }
                    }
                    unbind_function();
                    teardown_data_endpoints();
                    GadgetDriver::reset(gadget_mut());
                    udc_mut().reset();
                    CONFIGURED = false;
                    DATA_ENDPOINTS_READY = false;
                    DATA_REQUEST_SLOTS = [usize::MAX; 2];
                    DATA_RESOURCE_INDEX = [0; 2];
                    EP0_RESOURCE_INDEX = [0; 2];
                    EP0_SETUP_ARMED = false;
                    EP0_STATE = Ep0State::Setup;
                    CONTROL_IN = false;
                    CONTROL_HAS_DATA = false;
                    ENDPOINTS_READY = false;
                    write(DALEPENA, 0);
                }
                note_runtime_event(super::platform::bramble::UsbRuntimeEvent::Disconnect);
            }
            1 => {
                SIGNAL_USB_RESET_SEEN = true;
                trace_utmi_state(6);
                trace_event(TRACE_USB_RESET, 0, 0, 0, 0, raw);
                note_runtime_event(super::platform::bramble::UsbRuntimeEvent::BusReset);
                #[cfg(fullerene_aarch64_usb_gadget_handoff_usb3_link_training_after_reset)]
                unsafe {
                    // Android's dwc3_gadget_reset_interrupt() starts the
                    // optional QMP RX-equalization workaround before stopping
                    // active transfers. The helper is status-gated and is a
                    // no-op when the PHY is not in equalization.
                    let training = phy::qmp_start_link_training();
                    trace_event(
                        TRACE_USB_RESET,
                        0x514c_5453, // "QLTS": QMP link-training start
                        training,
                        0,
                        0,
                        read(DSTS),
                    );
                }
                #[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_clear_susphy_after_reset)]
                unsafe {
                    // qpr1's USB2 PHY suspend policy is revisited at the
                    // host-reset boundary. Keep this write isolated from the
                    // existing Run/Stop A/B so a reset-time PHY wake can be
                    // tested without changing the initial handoff.
                    let before = read(GUSB2PHYCFG0);
                    mark_g2w_site(1003);
                    write(GUSB2PHYCFG0, before & !GUSB2PHYCFG_SUSPHY);
                    let after = read(GUSB2PHYCFG0);
                    trace_event(
                        TRACE_USB_RESET,
                        0x53555352, // "SUSR": USB2 SUSPHY reset-boundary A/B
                        before,
                        after,
                        0,
                        read(DSTS),
                    );
                }
                unsafe { restart_control_after_reset() }
                #[cfg(fullerene_aarch64_usb_gadget_handoff_dalepena_after_reset)]
                unsafe {
                    // This is deliberately after the reset handler returns:
                    // it tests the publication boundary without rebuilding
                    // EP0 contexts, changing the SETUP TRB, or touching the
                    // PHY/UTMI path. If the bus reset cleared only the active
                    // endpoint mask, this is the narrowest recovery action.
                    let before = read(DALEPENA);
                    write(DALEPENA, 0b11);
                    let after = read(DALEPENA);
                    trace::live_dalepena_after_reset(before, after);
                    trace_event(
                        TRACE_USB_RESET,
                        0x4441_4C45, // "DALE"
                        before,
                        after,
                        0,
                        read(DSTS),
                    );
                }
                trace_utmi_state(7);
            }
            2 => {
                trace_event(TRACE_DEVICE_CONNECT, 0, 0, 0, 0, raw);
                let speed = unsafe { read(DSTS) & DSTS_CONNECTSPD_MASK };
                log_puts("usb: connect done, speed=");
                log_hex_value(speed as u64);
                if cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_source_exact_devten) {
                    // qpr1 adds EOPFEN (the revision-2.30a suspend event bit)
                    // here, after Connect Done and before EP0 MODIFY.
                    unsafe { qpr1_enable_eopf_on_connect_done() };
                }
                unsafe { configure_android_hs_connect_done_policy(speed) };
                #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_conndone_clear_hird)]
                if speed == DSTS_SUPERSPEED {
                    // qpr1's dwc3_gadget_conndone_interrupt() clears the
                    // HIRD threshold on the non-HS branch. The regular
                    // handoff leaves XBL's pre-connect value in DCTL; keep
                    // this source-aligned write isolated for an A/B.
                    unsafe {
                        let dctl = read(DCTL) & !DCTL_HIRD_THRES_MASK;
                        write(DCTL, dctl);
                        let _ = read(DCTL);
                    }
                }
                unsafe {
                    CONNECT_TICK = arch_counter();
                    PENDING_SETUP_ARM = true;
                }
                // Linux's DWC3 gadget driver starts with the SuperSpeed EP0
                // size and modifies it after Connect Done.
                let max_packet = if speed == DSTS_SUPERSPEED { 512 } else { 64 };
                unsafe {
                    let first_connect = !ENDPOINTS_READY;
                    // A post-reset Connect Done (first_connect false) must not
                    // rewrite DALEPENA or re-arm while EP0 holds an armed
                    // SETUP TRB. Linux's conndone path does, however, issue a
                    // DEPCFG MODIFY for both EP0 directions so a USB2 link
                    // changes the initial 512-byte state to 64 bytes. Leaving
                    // the pre-connect packet size in place makes the host's
                    // first descriptor transaction use the wrong EP0 context.
                    if !first_connect && EP0_STATE == Ep0State::Setup && EP0_SETUP_ARMED {
                        let modified = configure_endpoint(0, max_packet, true)
                            && configure_endpoint(1, max_packet, true);
                        if modified {
                            let _ = udc_mut().configure_endpoint(0, max_packet as u16, true);
                            let _ = udc_mut().configure_endpoint(1, max_packet as u16, true);
                            note_runtime_event(
                                super::platform::bramble::UsbRuntimeEvent::ControllerStarted,
                            );
                        } else {
                            log_puts("usb: Connect Done EP0 MODIFY failed\n");
                            trace_event(
                                TRACE_USB_DEVICE_ERROR,
                                0x434D_4F44, // "CMOD"
                                max_packet,
                                0,
                                0,
                                read(DSTS),
                            );
                        }
                        return;
                    }
                    let endpoints_ready = if first_connect {
                        configure_endpoint(0, max_packet, false)
                            && configure_endpoint(1, max_packet, false)
                    } else {
                        configure_endpoint(0, max_packet, true)
                            && configure_endpoint(1, max_packet, true)
                    };
                    if endpoints_ready {
                        ENDPOINTS_READY = true;
                        let _ = udc_mut().configure_endpoint(0, max_packet as u16, false);
                        let _ = udc_mut().configure_endpoint(1, max_packet as u16, false);
                        write(DALEPENA, 0b11);
                        // The two Bramble timing differentials own the first
                        // EP0 STARTTRANSFER at a different boundary. Do not
                        // issue a second STARTTRANSFER at Connect Done: the
                        // host's USB RESET path will revoke the old resource
                        // and arm the fresh SETUP transfer exactly once.
                        if !cfg!(any(
                            fullerene_aarch64_usb_gadget_handoff_start_after_connect,
                            fullerene_aarch64_usb_gadget_handoff_start_after_reset
                        )) {
                            rearm_setup();
                        }
                        note_runtime_event(
                            super::platform::bramble::UsbRuntimeEvent::ControllerStarted,
                        );
                    }
                }
            }
            DEVICE_EVENT_LINK_STATUS_CHANGE => {
                // The Qualcomm glue consumes link changes for its LPM/PHY
                // policy.  Keep the event visible in retained RAM even when
                // this early gadget has no negotiated LPM policy of its own.
                trace_event(TRACE_LINK_STATUS, 0, 0, 0, 0, raw);
            }
            DEVICE_EVENT_WAKEUP => {
                trace_event(TRACE_USB_WAKEUP, 0, 0, 0, 0, raw);
                // The normal Linux path queues resume work from the wakeup
                // event. Keep the same boundary here; process_event() may be
                // reached from the synchronous early IRQ dispatcher.
                unsafe {
                    RESUME_PENDING = true;
                }
            }
            DEVICE_EVENT_SUSPEND => {
                // DWC3 emits a suspend event during initial attach on some
                // revisions, before RESET/CONNECT_DONE and before the gadget
                // is configured. Linux deliberately ignores that event.
                // Once configured, this is still the USB bus entering L1/L2,
                // not a system runtime-PM request. Do not power-gate the
                // Qualcomm USB clock/rails here: doing so tears down a live
                // gadget and makes a successful enumeration disappear.
                let configured = unsafe { CONFIGURED };
                if configured {
                    trace_event(TRACE_USB_SUSPEND, 0, 0, 0, 0, raw);
                }
            }
            DEVICE_EVENT_HIBERNATION_REQUEST => {
                trace_event(TRACE_USB_DEVICE_ERROR, device_event, 0, 0, 0, raw);
                // A DWC3 hibernation notification is not by itself a system
                // suspend request. Keep the Qualcomm session powered while
                // the host keeps the SuperSpeed gadget idle; powering down
                // here makes a successfully configured bulk gadget disappear.
                // Explicit runtime suspend/resume remains available to the
                // platform policy, but this hardware event alone must not
                // invoke it.
            }
            DEVICE_EVENT_ERRATIC_ERROR | DEVICE_EVENT_CMD_COMPLETE | DEVICE_EVENT_OVERFLOW => {
                SIGNAL_DWC3_DEVICE_ERROR = true;
                trace_event(TRACE_USB_DEVICE_ERROR, device_event, 0, 0, 0, raw);
            }
            _ => {}
        }
        return;
    }

    let endpoint = ((raw >> 1) & 0x1f) as usize;
    let event = (raw >> 6) & 0xf;
    let status = (raw >> 12) & 0xf;
    if event == 1 {
        if endpoint >= 2 {
            unsafe { complete_bulk_transfer(endpoint, status, raw) };
            return;
        }
        // Linux's dwc3_ep0_xfer_complete() does NOT look at the event status
        // at all: XferComplete status bits on EP0 carry LST/IOC-style flags
        // (our SETUP TRB sets LST, so a healthy completion reports 0x8), and
        // the dispatch is purely by ep0state. Routing non-zero statuses into
        // the recovery path would eat every healthy SETUP completion.
        unsafe {
            EP0_RESOURCE_INDEX[endpoint] = 0;
            // The previously armed SETUP/DATA/STATUS transfer is consumed;
            // the poll-loop guard re-arms the SETUP TRB once EP0 returns to
            // the Setup state.
            EP0_SETUP_ARMED = false;
            if EP0_STATE == Ep0State::Setup {
                // EP0 XferComplete status 0x8 is the normal LST indication,
                // not an error. The request carries the same successful
                // setup completion in the XBL path.
                complete_xbl_setup_request(false);
            }
            // A freshly DMAed SETUP packet overrides any in-flight phase:
            // hosts abort stalled control transfers by sending a new SETUP,
            // and the completion event for the OLD transfer carries it.
            // Linux recovers via setup_packet_pending; without this the new
            // SETUP is dispatched into the stale Data/Status handler and the
            // request is silently lost (the mid-enumeration death).
            let setup = ep0_setup_data_ptr();
            cache_invalidate(setup as usize, 8);
            // The source-aligned Bramble path aliases the SETUP payload to
            // TRB0.  In that layout the idle TRB already contains its DMA
            // address, so an "any non-zero byte" test reports a fresh SETUP
            // on every completed transfer and feeds TRB metadata back into
            // handle_setup().  Treat a changed bpl/bph pair as the payload
            // marker, matching the retained-trace and DMA fallback tests.
            let fresh_setup = if setup == ep0_trb_ptr(0).cast::<u8>() {
                let expected = dma_iova_for(setup as usize);
                let current_low = read_volatile(setup.cast::<u32>());
                let current_high = read_volatile(setup.add(4).cast::<u32>());
                current_low != expected as u32 || current_high != (expected >> 32) as u32
            } else {
                let mut received = false;
                for offset in 0..8 {
                    if read_volatile(setup.add(offset)) != 0 {
                        received = true;
                        break;
                    }
                }
                received
            };
            if fresh_setup {
                EP0_STATE = Ep0State::Setup;
                handle_setup();
                return;
            }
        }
        trace_event(
            TRACE_TRANSFER_COMPLETE,
            event,
            endpoint as u32,
            status,
            0,
            raw,
        );
        unsafe {
            match EP0_STATE {
                Ep0State::Setup => handle_setup(),
                Ep0State::Data if endpoint == 0 || endpoint == 1 => {
                    let action = GadgetDriver::on_transfer_complete(gadget_mut());
                    EP0_STATE = Ep0State::Status;
                    match action {
                        ControlAction::StatusOut => {
                            if !start_status(0) {
                                stall_control(0);
                            }
                        }
                        ControlAction::StatusIn => {
                            if !start_status(1) {
                                stall_control(1);
                            }
                        }
                        _ => stall_control(if CONTROL_IN { 1 } else { 0 }),
                    }
                }
                Ep0State::Status => match GadgetDriver::on_transfer_complete(gadget_mut()) {
                    ControlAction::Setup => {
                        sync_gadget_state();
                        EP0_STATE = Ep0State::Setup;
                        rearm_setup();
                    }
                    ControlAction::SetHalt(address) => {
                        let endpoint = (address & 0x7f) as usize;
                        if send_ep_command(endpoint, DEPCMD_SETSTALL, 0, 0, 0)
                            && udc_mut().set_halt(address, true)
                        {
                            sync_gadget_state();
                            EP0_STATE = Ep0State::Setup;
                            rearm_setup();
                        } else {
                            stall_control(if CONTROL_IN { 1 } else { 0 });
                        }
                    }
                    ControlAction::ClearHalt(address) => {
                        let endpoint = (address & 0x7f) as usize;
                        if send_ep_command(endpoint, DEPCMD_CLEARSTALL, 0, 0, 0)
                            && udc_mut().set_halt(address, false)
                        {
                            sync_gadget_state();
                            EP0_STATE = Ep0State::Setup;
                            rearm_setup();
                        } else {
                            stall_control(if CONTROL_IN { 1 } else { 0 });
                        }
                    }
                    _ => stall_control(if CONTROL_IN { 1 } else { 0 }),
                },
                _ => {}
            }
        }
    } else if event == 3 {
        // XferNotReady: the core asks for the next phase's TRB. Record every
        // event for the harvest gates (request=endpoint, value=status).
        // XferNotReady is a notification for a later control phase, not the
        // initial SETUP arm point. The ordinary path has already armed EP0;
        // retain the event trace while handling DATA/STATUS below.
        trace_event(TRACE_XFER_NOT_READY, endpoint as u32, status, 0, 0, raw);
        if status == 1 && endpoint == 1 {
            unsafe {
                // CONTROL_DATA NRDY on EP1 IN is normally informational after
                // the immediate Android-style arm above. It remains a
                // recovery boundary only when the initial STARTTRANSFER was
                // rejected while the link was settling; the host keeps
                // tokening the data phase and the next event retries it.
                if DATA_PHASE_PENDING_START && EP0_STATE == Ep0State::Data {
                    let length = DATA_PHASE_PENDING_LEN;
                    let trb_index = ep0_trb_index(1);
                    prepare_trb(
                        trb_index,
                        ep0_response_ptr(),
                        length,
                        if cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_ep0_in_data) {
                            TRB_NORMAL
                        } else {
                            TRB_CONTROL_DATA
                        },
                    );
                    let queued = retry_start_transfer(1, ep0_trb_ptr(trb_index), 200);
                    trace_event(
                        TRACE_DESCRIPTOR_QUEUED,
                        0x4441_524D, // "DARM" data-phase arm outcome
                        queued as u32,
                        0,
                        length as u32,
                        read(DSTS),
                    );
                    if queued {
                        DATA_PHASE_PENDING_START = false;
                        note_probe_ep0_progress();
                    }
                }
            }
        }
        if status == 2 {
            unsafe {
                if EP0_STATE == Ep0State::Status {
                    let endpoint = if CONTROL_HAS_DATA && CONTROL_IN { 0 } else { 1 };
                    if !start_status(endpoint) {
                        stall_control(endpoint);
                    }
                }
            }
        }
    }
}

/// Recover EP0 after a non-success transfer-complete status.
///
/// DWC3 can report a completed control transfer with an error status when a
/// host aborts the request, the link changes, or the controller loses the
/// transfer resource during a handoff. Linux removes the old request before
/// queueing the next SETUP; treating the event as a normal Data/Status
/// transition would instead leave EP0 pointing at a retired TRB and produce
/// another host timeout. Revoke the resource first, clear the software state,
/// and rearm SETUP only after the endpoint ownership boundary is restored.
unsafe fn recover_control_transfer(endpoint: usize, status: u32, raw: u32) {
    trace_event(
        TRACE_USB_DEVICE_ERROR,
        endpoint as u32,
        raw,
        status,
        EP0_STATE as u32,
        read(DSTS),
    );
    if endpoint < 2 && EP0_RESOURCE_INDEX[endpoint] != 0 {
        let _ = end_transfer(endpoint);
        EP0_RESOURCE_INDEX[endpoint] = 0;
    }
    EP0_STATE = Ep0State::Setup;
    CONTROL_IN = false;
    CONTROL_HAS_DATA = false;
    if ENDPOINTS_READY {
        let _ = rearm_setup();
    }
}

unsafe fn complete_bulk_transfer(endpoint: usize, status: u32, raw: u32) {
    if endpoint != 2 && endpoint != 3 {
        return;
    }
    let index = endpoint - 2;
    let slot = unsafe { DATA_REQUEST_SLOTS[index] };
    if slot == usize::MAX {
        trace_event(TRACE_USB_DEVICE_ERROR, endpoint as u32, raw, 0, 0, status);
        return;
    }
    let address = if endpoint == 3 { 0x83 } else { 0x02 };
    unsafe {
        let trb = addr_of_mut!(DATA_TRBS).cast::<Trb>().add(index);
        cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
        let residual = read_volatile(addr_of!((*trb).size)) & 0x00ff_ffff;
        let actual = udc_mut()
            .request(address, slot)
            .map(|request| request.length.saturating_sub(residual))
            .unwrap_or(0);
        let error = status != 0;
        let _ = udc_mut().complete(address, slot, actual, error);
        if endpoint == 2 {
            let data = core::slice::from_raw_parts(
                addr_of!(DATA_OUT_BUFFER.0).cast::<u8>(),
                actual as usize,
            );
            debug_transport::on_bulk_out(data, error);
        } else {
            debug_transport::on_bulk_in_complete(error);
        }
        GadgetDriver::on_data_complete(gadget_mut(), address, actual, error);
        trace_event(
            TRACE_TRANSFER_COMPLETE,
            endpoint as u32,
            raw,
            status,
            actual,
            error as u32,
        );
        let _ = udc_mut().release(address, slot);
        DATA_REQUEST_SLOTS[index] = usize::MAX;
        DATA_RESOURCE_INDEX[index] = 0;
        // Keep an OUT request posted after completion. This is the bounded
        // early-boot equivalent of a gadget function's request callback
        // requeue; the release above returns the UDC slot before reuse.
        if endpoint == 2 && CONFIGURED && DATA_ENDPOINTS_READY {
            let _ = queue_bulk_transfer(
                2,
                addr_of_mut!(DATA_OUT_BUFFER.0).cast::<u8>(),
                DATA_OUT_BUFFER_SIZE,
            );
        }
    }
}

/// Initialize the Bramble DWC3 in peripheral mode and connect the pull-up.
pub fn init() -> bool {
    super_speed::init_with_super_speed(true, true, true)
}

/// Initialize only the USB2 path for the dependency-free hardware probe.
pub fn init_usb2_only() -> bool {
    super_speed::init_with_super_speed(false, true, true)
}

/// Pass the PMIC/Type-C cable orientation into the QMP combo PHY path.
/// Android programs the QMP Type-C control register after the PHY is powered
/// and before releasing the combo-PHY reset override.
pub fn set_typec_orientation(orientation_reverse: bool) {
    unsafe {
        TYPEC_LANE_B = orientation_reverse;
    }
}

/// Install the PMIC state discovered before the controller is touched. The
/// APID is retained so later PDC/GIC events can refresh the same Type-C
/// peripheral without another arbiter tree walk.
pub fn install_typec_state(state: super::platform::bramble::TypecState) {
    unsafe {
        TYPEC_STATE = state;
        TYPEC_STATE_VALID = true;
        TYPEC_POLL_TICKS = 0;
    }
}

pub fn note_platform_powered() {
    unsafe {
        USB_RUNTIME_STATE = super::platform::bramble::usb_runtime_transition(
            USB_RUNTIME_STATE,
            super::platform::bramble::UsbRuntimeEvent::PlatformPowered,
        );
    }
}

pub fn note_typec_attached(attached: bool) {
    if !attached {
        return;
    }
    unsafe {
        USB_RUNTIME_STATE = super::platform::bramble::usb_runtime_transition(
            USB_RUNTIME_STATE,
            super::platform::bramble::UsbRuntimeEvent::TypecAttached,
        );
    }
}

/// Observe the Type-C state at the Fastboot handoff boundary without
/// changing PMIC registers.  Android obtains this state through the
/// Qualcomm role-switch/PMIC driver before it starts the UDC; the temporary
/// image has no role-switch framework, so perform the same read-only bridge
/// explicitly.  A failed observation is non-fatal: Fastboot has already
/// established a device-mode transport, and the later DWC3/EP0 probe must
/// remain useful for separating an SPMI aperture problem from a USB problem.
pub fn observe_typec_handoff() -> bool {
    note_runtime_event(super::platform::bramble::UsbRuntimeEvent::PlatformPowered);
    trace_marker(TRACE_TYPEC_BEGIN, 0x4f4253); // "OBS"
    let Some(state) = (unsafe { super::platform::bramble::observe_usb_device_role() }) else {
        trace_marker(TRACE_TYPEC_DONE, 0xffff_ffff);
        return false;
    };

    set_typec_orientation(state.orientation_reverse);
    note_typec_attached(state.attached);
    trace_event(
        TRACE_TYPEC_DONE,
        state.role as u32,
        state.attached as u32,
        state.orientation_reverse as u32,
        state.mode as u32,
        state.misc_status as u32,
    );
    unsafe {
        TYPEC_STATE = state;
        TYPEC_STATE_VALID = true;
        TYPEC_POLL_TICKS = 0;
    }
    true
}

/// Complete a deferred Type-C parent interrupt outside the hard IRQ entry.
/// This mirrors Linux's threaded qpnpint/role-switch boundary.
pub fn service_deferred_platform() {
    unsafe {
        if !TYPEC_IRQ_PENDING {
            return;
        }
        if !TYPEC_STATE_VALID {
            // The standalone gadget probe intentionally skips SPMI role
            // discovery. Leave the diagnostic parent SPI masked rather than
            // issuing an acknowledge against an uninitialized PMIC state.
            TYPEC_IRQ_PENDING = false;
            return;
        }
        // Linux's PMIC Type-C handler samples the child state in its threaded
        // context before clearing the parent summary. Do the same here: an
        // acknowledge-only path loses a real attach/detach edge and leaves
        // the DWC3 session in the previous role.
        let event = {
            let state = &mut *addr_of_mut!(TYPEC_STATE);
            let event = super::platform::bramble::refresh_usb_device_role(state);
            if event.is_some() {
                TYPEC_LANE_B = state.orientation_reverse;
            }
            event
        };
        if let Some(event) = event {
            apply_typec_event(event);
        }
        let state = &*addr_of!(TYPEC_STATE);
        if !super::platform::bramble::acknowledge_typec_irq(state) {
            trace_event(
                TRACE_USB_DEVICE_ERROR,
                super::platform::bramble::usb_typec_parent_irq(),
                0,
                0,
                0,
                0,
            );
        }
        TYPEC_IRQ_PENDING = false;
        super::platform::gicv3::enable_spis(
            super::platform::bramble::GICD_BASE,
            &[super::platform::bramble::usb_typec_parent_irq()],
        );
    }
}

fn note_runtime_event(event: super::platform::bramble::UsbRuntimeEvent) {
    unsafe {
        USB_RUNTIME_STATE =
            super::platform::bramble::usb_runtime_transition(USB_RUNTIME_STATE, event);
    }
}

unsafe fn apply_typec_event(event: super::platform::bramble::TypecEvent) {
    trace_event(TRACE_TYPEC_EVENT, event as u32, 0, 0, 0, 0);
    match event {
        super::platform::bramble::TypecEvent::DetachDetected => {
            // Linux's role-switch callback stops advertising before it tears
            // down the UDC queues. Do not issue endpoint commands after the
            // PMIC has removed the cable.
            TYPEC_DETACH_SEEN = true;
            unbind_function();
            teardown_data_endpoints();
            reset_gsi_channels();
            write(DALEPENA, 0);
            let _ = run_stop_device(false);
            ENDPOINTS_READY = false;
            CONFIGURED = false;
            DATA_ENDPOINTS_READY = false;
            DATA_REQUEST_SLOTS = [usize::MAX; 2];
            DATA_RESOURCE_INDEX = [0; 2];
            GadgetDriver::reset(gadget_mut());
            udc_mut().reset();
            note_runtime_event(super::platform::bramble::UsbRuntimeEvent::Disconnect);
        }
        super::platform::bramble::TypecEvent::HostDetected => {
            // The PMIC role-switch may move directly from device to source
            // when another Type-C partner is attached. A source/host role
            // must never leave the old gadget pull-up or DMA request live.
            unbind_function();
            teardown_data_endpoints();
            reset_gsi_channels();
            write(DALEPENA, 0);
            let _ = run_stop_device(false);
            ENDPOINTS_READY = false;
            CONFIGURED = false;
            DATA_ENDPOINTS_READY = false;
            DATA_REQUEST_SLOTS = [usize::MAX; 2];
            DATA_RESOURCE_INDEX = [0; 2];
            GadgetDriver::reset(gadget_mut());
            udc_mut().reset();
            note_runtime_event(super::platform::bramble::UsbRuntimeEvent::Disconnect);
        }
        super::platform::bramble::TypecEvent::AttachDetected => {
            // Attach is the prerequisite for the Qualcomm VBUS/session
            // override. Connect Done will reconfigure EP0 and rearm SETUP
            // when the host starts the new USB session.
            note_runtime_event(super::platform::bramble::UsbRuntimeEvent::TypecAttached);
            qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
            qscratch_set(QSCRATCH_HS_PHY_CTRL, (1 << 20) | (1 << 28));
        }
        _ => {}
    }
}

/// Enable the Qualcomm glue notifications that the Android driver consumes.
/// The DWC3 event ring does not report P3/L1 transitions, so leaving this mask
/// at the bootloader default makes runtime-PM state diverge even when EP0 is
/// functioning.
unsafe fn enable_power_events() {
    let mut mask = unsafe { read_qscratch(QSCRATCH_PWR_EVENT_MASK) };
    if cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_source_power_events) {
        // qpr1 enables only the initial P3-enter notification at the core
        // resume boundary.  OUT_P3/L1-out are added later by the SS wake and
        // DBM paths when those paths actually own a low-power transition.
        mask |= PWR_EVENT_POWERDOWN_IN_P3;
    } else {
        mask |= PWR_EVENT_POWERDOWN_IN_P3 | PWR_EVENT_POWERDOWN_OUT_P3 | PWR_EVENT_LPM_OUT_L1;
    }
    unsafe { write_qscratch(QSCRATCH_PWR_EVENT_MASK, mask) };
}

#[inline]
const fn power_event_clear_mask(status: u32) -> u32 {
    // P3 and L1-out are edge notifications consumed by the Qualcomm glue.
    // L2-out is intentionally not included: the Android handler treats it as
    // an indication while the suspend path explicitly clears L2-in.
    status & (PWR_EVENT_POWERDOWN_IN_P3 | PWR_EVENT_POWERDOWN_OUT_P3 | PWR_EVENT_LPM_OUT_L1)
}

#[inline]
const fn power_event_requests_resume(status: u32) -> bool {
    status & PWR_EVENT_LPM_OUT_L1 != 0
}

/// Match the P3 bookkeeping in dwc3_pwr_event_handler().  When both bits are
/// reported the hardware does not identify the direction in the event word;
/// qpr1 resolves that ambiguity from the DWC3 link state.
#[inline]
unsafe fn update_p3_state(status: u32) {
    let p3_in = status & PWR_EVENT_POWERDOWN_IN_P3 != 0;
    let p3_out = status & PWR_EVENT_POWERDOWN_OUT_P3 != 0;
    if p3_in && !p3_out {
        USB_IN_P3 = true;
    } else if p3_out && !p3_in {
        USB_IN_P3 = false;
    } else if p3_in && p3_out && cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_source_power_events)
    {
        // DWC_usb31 encodes U3 as 0x03 in the link-state field, matching the
        // source driver's DWC3_LINK_STATE_U3 test.
        USB_IN_P3 = gdb_ltssm_link_state() == 0x03;
    }
}

/// Prepare the USB2 PHY for runtime suspend using the same observable
/// boundary as Android's dwc3_msm_prepare_suspend().  The early image has no
/// jiffies/workqueue, so the bounded loop is expressed in MMIO polling
/// iterations.  A device-mode failure is recorded but is non-fatal, matching
/// the upstream path for a non-host/non-bus-suspend transition.
unsafe fn prepare_usb2_suspend() -> bool {
    unsafe {
        // Clear stale L2 notifications before asking the PHY to enter L2.
        write_qscratch(
            QSCRATCH_PWR_EVENT_STATUS,
            PWR_EVENT_LPM_IN_L2 | PWR_EVENT_LPM_OUT_L2,
        );
        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 |= GUSB2PHYCFG_ENBLSLPM | GUSB2PHYCFG_SUSPHY;
        mark_g2w_site(1004);
        write(GUSB2PHYCFG0, usb2);
        let _ = read(GUSB2PHYCFG0);

        let mut entered_l2 = false;
        for _ in 0..1_000_000u32 {
            if read_qscratch(QSCRATCH_PWR_EVENT_STATUS) & PWR_EVENT_LPM_IN_L2 != 0 {
                entered_l2 = true;
                break;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }

        if !entered_l2 {
            trace_event(
                TRACE_USB_DEVICE_ERROR,
                0x4c_32544f,
                read_qscratch(QSCRATCH_PWR_EVENT_STATUS),
                read(GUSB2PHYCFG0),
                read(DSTS),
                0,
            );
        }

        // The status bit is W1C.  This is done even on the device-mode timeout
        // path, as in Android's prepare_suspend(), so a stale L2-in event does
        // not wake the next runtime transition immediately.
        write_qscratch(QSCRATCH_PWR_EVENT_STATUS, PWR_EVENT_LPM_IN_L2);
        entered_l2
    }
}

/// Drain the Qualcomm glue power-event status separately from DWC3 device
/// events. Android's threaded power IRQ handles P3/L1 transitions here; if
/// the early boot path has not yet installed a working GIC route, polling the
/// same W1C status register keeps the transition observable without confusing
/// a power event with an EP0 transfer event.
unsafe fn service_power_event() {
    let status = unsafe { read_qscratch(QSCRATCH_PWR_EVENT_STATUS) };
    if status == 0 || status == u32::MAX {
        return;
    }

    unsafe { update_p3_state(status) };
    trace_event(
        TRACE_USB_DEVICE_ERROR,
        0x5057_5200,
        status,
        unsafe { USB_IN_P3 as u32 },
        0,
        0,
    );
    if status & (PWR_EVENT_LPM_IN_L2 | PWR_EVENT_LPM_OUT_L2) != 0 {
        trace_event(
            TRACE_USB_DEVICE_ERROR,
            0x4c,
            status & (PWR_EVENT_LPM_IN_L2 | PWR_EVENT_LPM_OUT_L2),
            0,
            0,
            0,
        );
    }
    if power_event_requests_resume(status) {
        unsafe {
            RESUME_PENDING = true;
        }
    }
    // L2-out is an indication used by the Qualcomm state machine; Linux
    // deliberately leaves it in the status value while processing the
    // transition. Do not write it back as W1C here.
    let clear = power_event_clear_mask(status);
    if clear != 0 {
        unsafe { write_qscratch(QSCRATCH_PWR_EVENT_STATUS, clear) };
    }
}

/// Poll the PMIC Type-C status at a bounded rate. This covers the interval
/// before a stable GIC owner exists; the IRQ path calls the same operation
/// immediately for USB-related parent interrupts.
unsafe fn poll_typec_state(force: bool) {
    if !TYPEC_STATE_VALID {
        return;
    }
    // Before the GIC/PMIC child IRQ route is live, bounded polling bridges
    // the handoff gap. Once Linux's normal role-change interrupt boundary is
    // installed, keep the PMIC read on that IRQ path only; polling every USB
    // event can sample a transient CC state and falsely apply detach to a
    // live gadget.
    if super::platform::bramble::usb_resource_state().irq_routes_enabled {
        return;
    }
    TYPEC_POLL_TICKS = TYPEC_POLL_TICKS.wrapping_add(1);
    if !force && TYPEC_POLL_TICKS & 0x3fff != 0 {
        return;
    }
    let state = unsafe { &mut *addr_of_mut!(TYPEC_STATE) };
    if let Some(event) = unsafe { super::platform::bramble::refresh_usb_device_role(state) } {
        TYPEC_LANE_B = state.orientation_reverse;
        unsafe { apply_typec_event(event) };
    }
}

/// Entry point used by the AArch64 IRQ dispatcher for Qualcomm power and PDC
/// parent lines. A PMIC event is kept separate from a DWC3 event-buffer word.
pub fn handle_platform_irq(interrupt_id: u32) {
    unsafe {
        trace_event(TRACE_PLATFORM_IRQ, interrupt_id, 0, 0, 0, 0);
        if super::platform::bramble::is_usb_smmu_irq(interrupt_id) {
            service_smmu_fault();
        }
        if interrupt_id == super::platform::bramble::usb_power_event_irq() {
            service_power_event();
        }
        if interrupt_id == super::platform::bramble::usb_typec_parent_irq() {
            // The initial role request above is authoritative for a
            // fastboot handoff.  The PMIC parent can deliver a stale
            // transition while Fastboot tears down its gadget; re-reading
            // MISC_STATUS here would turn that transient into a false
            // detach and remove the live Fullerene pull-up. Mark the parent
            // pending here; the SPMI child clear runs in the normal
            // processing context, like Linux's threaded qpnpint/role-switch
            // path.
            TYPEC_IRQ_PENDING = true;
        }
    }
}

/// Enter the same controller-side runtime suspend boundary as the Qualcomm
/// glue: drain GSI write state, stop the device, and only then allow the
/// platform vote to fall to the suspend case. The PM QoS/interconnect payload
/// is resolved by the platform resource contract; firmware-owned vote writes
/// are intentionally kept outside this MMIO-only early path.
pub fn runtime_suspend() -> bool {
    unsafe {
        if !set_gsi_doorbell_blocked(true) {
            return false;
        }
        if !gsi_ready_to_suspend() {
            let _ = set_gsi_doorbell_blocked(false);
            return false;
        }
        let _ = prepare_usb2_suspend();
        if QMP_PHY_READY {
            // The QMP driver keeps the connected SuperSpeed PHY powered and
            // switches it to autonomous receiver/LFPS detection before its
            // clocks are gated. This is separate from the USB2 L2 request.
            qmp_set_autonomous_mode(true);
        }
        if !run_stop_device(false) {
            let _ = set_gsi_doorbell_blocked(false);
            return false;
        }
        // Qualcomm enables pwr_event only as a low-power wake source. Mask it
        // before collapsing the USB clock/power domain so a stale status bit
        // cannot re-enter the active transition while the domain is closing.
        let _ = super::platform::bramble::set_usb_power_event_irq_enabled(false);
        suspend_data_transfers();
        suspend_gsi_transfers();
        udc_mut().suspend();
        note_runtime_event(super::platform::bramble::UsbRuntimeEvent::Suspend);
        if !super::platform::bramble::apply_usb_performance(
            super::platform::bramble::UsbBusVote::Suspend,
        ) {
            log_puts("usb: RPMh suspend vote unavailable\n");
        }
        if QMP_PHY_READY {
            if !super::platform::bramble::disable_usb30_gdsc() {
                log_puts("usb: USB3 GDSC collapse not observable\n");
            }
        }
        if !super::platform::bramble::disable_usb_clock_branches() {
            log_puts("usb: USB clock gate readback unavailable\n");
        }
        if !super::platform::bramble::apply_usb_power(false, QMP_PHY_READY) {
            log_puts("usb: RPMh regulator disable unavailable\n");
        }
        return true;
    }
    false
}

/// Resume the device controller after runtime suspend and reassert the
/// Qualcomm session-valid override before Run/Stop, matching the upstream
/// run/stop notifier ordering.
pub fn runtime_resume() -> bool {
    unsafe {
        if !super::platform::bramble::apply_usb_power(true, QMP_PHY_READY) {
            log_puts("usb: RPMh regulator enable unavailable\n");
        }
        if !super::platform::bramble::enable_usb30_gdsc() {
            log_puts("usb: USB3 GDSC restore not observable\n");
        }
        if !super::platform::bramble::enable_usb_clock_branches() {
            log_puts("usb: USB clock ungate readback unavailable\n");
        }
        if !super::platform::bramble::apply_usb_performance(
            super::platform::bramble::UsbBusVote::Nominal,
        ) {
            log_puts("usb: RPMh nominal vote unavailable\n");
        }
        // The pwr_event line is the Qualcomm low-power wake/resume boundary;
        // re-enable it only after the controller clocks and power domain are
        // live again, matching the Android glue's resume order.
        let _ = super::platform::bramble::set_usb_power_event_irq_enabled(true);
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(QSCRATCH_HS_PHY_CTRL, (1 << 20) | (1 << 28));
        enable_power_events();
        if QMP_PHY_READY {
            qmp_set_autonomous_mode(false);
        }
        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        mark_g2w_site(1005);
        write(GUSB2PHYCFG0, usb2);
        let _ = read(GUSB2PHYCFG0);
        let _ = set_gsi_doorbell_blocked(false);
        if !run_stop_device(true) {
            return false;
        }
        udc_mut().resume();
        if DATA_ENDPOINTS_READY {
            let _ = queue_bulk_transfer(
                2,
                addr_of_mut!(DATA_OUT_BUFFER.0).cast::<u8>(),
                DATA_OUT_BUFFER_SIZE,
            );
        }
        if GSI_GADGET_BOUND {
            GadgetDriver::on_gsi_channel_resume(gadget_mut());
        }
        note_runtime_event(super::platform::bramble::UsbRuntimeEvent::Resume);
        if ENDPOINTS_READY && !rearm_setup() {
            return false;
        }
        return true;
    }
    false
}

/// Take over the USB controller without resetting the PHY or clock branches.
/// Fastboot has already completed that hardware bring-up; resetting those
/// blocks during a `fastboot boot` handoff can remove the Type-C pull-up before
/// the new gadget has a chance to enumerate.
pub fn init_usb2_handoff() -> bool {
    screen_mark(0); // handoff entered - first screen timestamp of the run
    // pullup_mark(2) was here and produced no host edges (2026-09-29 09:10): at handoff entry the
    // DWC3 does not yet accept a run/stop. Removed so its 600 ms does not perturb the run; the
    // negative result is recorded in usb/README.md and the boundary-state ledger.
    // The first attempt must preserve Fastboot's secure-owned rails, clocks,
    // RPMh vote, and Type-C session. Reprogramming those resources underneath
    // the vendor controller can remove the pull-up before EP0 is ready.
    //
    // One thing this handoff CANNOT preserve is Fastboot's RPMh/interconnect
    // vote itself: it dies with the bootloader's exit, and ~25 seconds later
    // the USB clock branch collapses under the idle timer — every MMIO read
    // then faults with an asynchronous external abort and the exception
    // vector reboots the handset in the middle of host enumeration. Reassert
    // Fullerene's own votes up front (best-effort; the secure side may reject
    // individual transitions without making the handoff impossible).
    unsafe {
        let performance_vote = if cfg!(fullerene_aarch64_usb_gadget_handoff_core_hs_clock) {
            log_puts("usb: selecting Android HS core-clock performance state\n");
            super::platform::bramble::UsbBusVote::Svs
        } else {
            super::platform::bramble::UsbBusVote::Nominal
        };
        let performance = super::platform::bramble::usb_performance_state(performance_vote);
        if !super::platform::bramble::apply_usb_power(true, false) {
            log_puts("usb: RPMh USB PHY regulator vote unavailable; continuing\n");
        }
        let _ = super::platform::bramble::enable_usb30_gdsc();
        let _ = super::platform::bramble::apply_usb_performance(performance.vote);
        let _ = super::platform::bramble::usb_bus_vectors(performance.vote);
        // Latch the core id now, while the aperture answers, so no readout ever has to.
        latch_snpsid();
    }

    #[cfg(fullerene_aarch64_usb_gadget_handoff_super_speed)]
    {
        // A SuperSpeed direct handoff must enter the QMP/DWC3 USB3 path
        // directly.  Falling through to the generic USB2 attempt first can
        // return true after publishing only the HS pull-up, which prevents
        // `init_usb2_gadget_handoff()` from ever reaching its SuperSpeed
        // initialization and makes an SS run look like a PHY failure.
        return init_usb2_gadget_handoff();
    }

    #[cfg(all(
        fullerene_aarch64_usb_gadget_handoff_probe,
        not(fullerene_aarch64_usb_gadget_handoff_super_speed)
    ))]
    {
        if init_usb2_gadget_handoff() {
            return true;
        }
        // Attribution control: do not let another initializer turn a failed
        // direct EP0 path into an indistinguishable physical attach. Unlike
        // SINGLE_ATTEMPT, this policy does not skip Run/Stop readback.
        if option_env!("FULLERENE_AARCH64_USB_DIRECT_ONLY") == Some("1") {
            return false;
        }
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if option_env!("FULLERENE_USB_SIGNAL_DMA_POST_RUNSTOP") == Some("1") {
            // Keep the post-link DMA diagnostic one-shot. Falling through to
            // the SuperSpeed fallback would publish a second controller path
            // and make a host-visible attach ambiguous.
            return false;
        }
    }

    // A QMP phase-stop run is a same-boot reachability measurement. If the
    // selected marker was not reached, do not fall through to the ordinary
    // USB2 path: that path would create the same HS attach as a reached-phase
    // fallback and make the phase-8 result ambiguous.
    if qmp_phase_probe_requested() {
        return false;
    }

    if super_speed::init_with_super_speed(false, true, false) {
        return true;
    }

    // A gate readout run must inspect the direct path's OWN failure state
    // while the ~17 s watchdog is still silent; the fallback's resets would
    // both consume the remaining window and overwrite the core state under
    // test. The caller's single-attempt limit then keeps the observation
    // window short enough to beat the bite.
    if option_env!("FULLERENE_USB_PROBE_SINGLE_ATTEMPT") == Some("1") {
        return false;
    }

    // Only after the non-destructive handoff fails do we attempt the complete
    // Qualcomm platform sequence. The caller may then use the cold USB2 path
    // as an explicit diagnostic of missing platform ownership.
    init_usb2_gadget_handoff()
}

/// Connect only the physical USB2 pull-up during a Fastboot handoff.
///
/// This diagnostic intentionally does not touch the event ring, endpoint
/// commands, or SMMU. It answers the narrower hardware question first: can
/// the Qualcomm PHY and DWC3 device controller make the port visible after
/// the bootloader disconnects? A host may report an incomplete USB device
/// because EP0 is not configured; that is expected for this probe.
pub fn init_usb2_pullup_handoff() -> bool {
    unsafe {
        log_hex("usb pullup: DWC3 GSNPSID=", read(GSNPSID) as u64);

        // Qualcomm's glue asserts LANE0_PWR_PRESENT together with the HS
        // VBUS/session override when entering peripheral mode. This is also
        // required on the USB2-only handoff path; it is not gated on QMP PHY
        // calibration in the Linux role-switch path.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        qscratch_set(QSCRATCH_CGCTL, 0x18);

        let mut gctl = read(GCTL);
        gctl &= !GCTL_PRTCAPDIR_MASK;
        gctl |= GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG;
        write(GCTL, gctl);

        if !device_soft_reset() {
            log_puts("usb pullup: DWC3 device reset failed\n");
            return false;
        }
        configure_dwc3_global_control();

        // This probe also resets the controller, so keep the historical
        // Fullerene controller-timing experiment at this boundary unless the
        // source-confirmed preserve-state A/B disables it.
        select_utmi_pipe_clock();
        update_dwc3_ref_clock();

        qscratch_set(
            QSCRATCH_SS_PHY_CTRL,
            1 << 24, // LANE0_PWR_PRESENT
        );
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        qscratch_set(QSCRATCH_CGCTL, 0x18);
        qscratch_set(QSCRATCH_GENERAL_CFG, QSCRATCH_GENERAL_CFG_XHCI_REV);
        // Fastboot handoff skips the cold-start clock setup above, but a
        // USB2-only device still needs Qualcomm's UTMI-as-PIPE selection.
        // Linux performs this during the DWC3 post-reset callback.
        select_utmi_pipe_clock();

        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        mark_g2w_site(1006);
        write(GUSB2PHYCFG0, usb2);
        let mut usb3 = read(GUSB3PIPECTL0);
        usb3 |= GUSB3PIPECTL_SUSPHY;
        write(GUSB3PIPECTL0, usb3);

        write(DCFG, DCFG_HIGHSPEED);
        write(DALEPENA, 0b11);
        // Fastboot leaves the USB2 link in its old negotiated state. Apply
        // the upstream RxDetect workaround only when GSNPSID identifies a
        // DWC3 revision for which that workaround is specified.
        // Keep the Qualcomm glue's VBUS/session override adjacent to the
        // connect transition, matching dwc3_qcom_run_stop_notifier().
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        if unsafe { run_stop_device(true) } {
            log_puts("usb pullup: DWC3 RUN/STOP active\n");
            return true;
        }
        log_hex("usb pullup: DWC3 remained halted, DSTS=", read(DSTS) as u64);
        false
    }
}

/// Perform only the writes needed to request a USB2 device pull-up.
///
/// This is intentionally a last-resort diagnostic. It avoids UART, DWC3
/// reset, event rings, endpoint commands, and SMMU access. The QSCRATCH VBUS
/// writes still use the Qualcomm glue's read-modify-write/readback sequence;
/// that ordering is part of the physical connect contract. If this does not
/// make the phone visible on the host, the failure is below the normal gadget
/// path: entry/exception handling, the Qualcomm USB glue, the PHY/session
/// state, or the bootloader's USB handoff itself.
/// Bare-pullup bisection checkpoint selector: 1 = PHY/session votes +
/// USB2 PHY wake only, 2 = +UTMI-as-PIPE clock mux, 3 = +GCTL/DCFG/DALEPENA,
/// absent = the full sequence through the Run/Stop start. The bare probe
/// parks after the checkpoint, so the host-visible attach time is the
/// cumulative cost of the executed prefix: it separates the
/// ABL-to-kernel-entry latency from the per-step controller cost.
fn bare_pullup_stop_after() -> Option<u32> {
    option_env!("FULLERENE_USB_BARE_PULLUP_STOP_AFTER").and_then(|value| value.parse::<u32>().ok())
}

unsafe fn init_usb2_bare_pullup_handoff_inner(connect: bool) -> bool {
    unsafe {
        // Bisection: a mark at the top of this body was SILENT 2/2 (2026-09-29 ~11:55), so the
        // transition is later in the function. The next mark is in the middle of the glue writes.
        // Match dwc3_qcom_vbus_override_enable(). qpr1 writes the
        // SuperSpeed lane power-present vote only when maximum_speed is at
        // least USB_SPEED_SUPER; the DCFG_FULLSPEED A/B is the corresponding
        // no-SS branch, so do not publish an SS-side session here.
        if !cfg!(any(
            fullerene_aarch64_usb_dcfg_fullspeed,
            fullerene_aarch64_usb_dcfg_lowspeed,
            fullerene_aarch64_usb_no_ss_vbus
        )) {
            qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24); // LANE0_PWR_PRESENT
        }
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        qscratch_set(QSCRATCH_CGCTL, 0x18);
        // Bisection: a mark immediately before enable_power_events() was SILENT 3/3 (2026-09-29
        // ~12:10), as was the top of the body. The transition is later in the function.
        enable_power_events();
        // Fastboot may leave the core in the USB2 suspended state when it
        // tears down its gadget just before jumping to the temporary image.
        // Waking the UTMI block is still below the EP0/DMA boundary and is
        // required before DCTL.Run/Stop can produce a new pull-up.
        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        mark_g2w_site(1007);
        write(GUSB2PHYCFG0, usb2);
        let _ = read(GUSB2PHYCFG0);
        let mut usb3 = read(GUSB3PIPECTL0);
        usb3 |= GUSB3PIPECTL_SUSPHY;
        write(GUSB3PIPECTL0, usb3);
        let _ = read(GUSB3PIPECTL0);
        let general = read_qscratch(QSCRATCH_GENERAL_CFG);
        write_qscratch(
            QSCRATCH_GENERAL_CFG,
            general | QSCRATCH_GENERAL_CFG_XHCI_REV,
        );
        // Bisection checkpoint 1: everything above is the PHY/session side
        // (Qualcomm glue votes + USB2 PHY wake). Stopping here tests whether
        // the Fastboot-inherited controller state already advertises the
        // pull-up once the PHY votes land; an early attach then measures the
        // ABL-to-first-MMIO latency alone.
        if let Some(stop) = bare_pullup_stop_after() {
            if stop == 1 {
                return true;
            }
        }
        // The bare path intentionally skips DWC3 reset, but it still needs
        // the Qualcomm glue's UTMI-as-PIPE clock selection when the Fastboot
        // session did not leave that mux configured for the temporary image.
        select_utmi_pipe_clock();
        // Bisection checkpoint 2: + the UTMI-as-PIPE clock mux (the 2x100 us
        // clock-source transitions are the largest fixed cost so far).
        if let Some(stop) = bare_pullup_stop_after() {
            if stop == 2 {
                return true;
            }
        }

        // Bisection: a mark immediately before the GCTL device-mode write was SILENT 3/3
        // (2026-09-29 ~12:25). The transition is later in the function.
        let gctl = read(GCTL);
        write(
            GCTL,
            (gctl & !GCTL_PRTCAPDIR_MASK) | GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG,
        );
        let _ = read(GCTL);
        // Bisection: a mark immediately after the GCTL PRTCAPDIR -> DEVICE write was SILENT 3/3
        // (2026-09-29 ~13:10), so the device-mode write is not the transition after all. Window is
        // now (:6971, :6992).
        configure_dwc3_global_control();
        // Final split (2026-09-29): between configure_dwc3_global_control() and the DCFG speed write
        // - the last two candidates. Silent here means the DCFG write is the transition; audible
        // means configure_dwc3_global_control() is. Gated on !connect for the :7167 caller.
        if !connect {
            pullup_mark(2);
        }
        write(DCFG, DCFG_HIGHSPEED);
        let _ = read(DCFG);
        // Bisection: a mark immediately after the DCFG speed write was AUDIBLE 3/3 (2026-09-29
        // ~13:25). Window is now (:6971, :6976) - configure_dwc3_global_control() and the DCFG
        // write, five lines. The mark moves between them for the final split.

        // Linux disables endpoint advertising before stopping the device
        // controller. In the ABL Stop() differential, the controller remains
        // live, so leave its endpoint advertisement untouched until the
        // later handoff sequence explicitly clears it.
        if connect || !cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_runstop) {
            write(DALEPENA, if connect { 0b11 } else { 0 });
            let _ = read(DALEPENA);
        }
        // Bisection checkpoint 3: + GCTL/DCFG/DALEPENA, before the VBUS
        // re-assert and the Run/Stop start. Stopping here isolates the
        // DCTL.Run/Stop wait as the only remaining cost between the last
        // plain MMIO and the host-visible attach.
        if let Some(stop) = bare_pullup_stop_after() {
            if stop == 3 {
                return true;
            }
        }

        // Bisection: a mark immediately before this glue re-assert was AUDIBLE 3/3 (2026-09-29
        // ~12:55). Window is now (:6963, :6992), and it contains the GCTL device-mode write.
        // Qualcomm's glue reasserts the VBUS override immediately before
        // enabling RUN_STOP so a stale Fastboot session cannot suppress the
        // connect-done transition.
        if !cfg!(any(
            fullerene_aarch64_usb_dcfg_fullspeed,
            fullerene_aarch64_usb_dcfg_lowspeed,
            fullerene_aarch64_usb_no_ss_vbus
        )) {
            qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        }
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        // Bisection: a mark immediately before this block was AUDIBLE 3/3 (2026-09-29 ~12:40), so
        // the transition is earlier in the function - window is now (:6963, :7008).
        // A gadget handoff uses the same proven PHY/session preparation but
        // keeps Run/Stop clear until its event ring and EP0 commands are
        // ready. The standalone bare probe requests the pull-up immediately.
        if connect {
            // The bare probe intentionally omits endpoint setup, but it still
            // uses the same PHY-safe Run/Stop boundary as Linux.
            run_stop_device(true)
        } else if cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_runstop) {
            log_puts("usb gadget handoff: preserving Fastboot Run/Stop state\n");
            true
        } else {
            run_stop_device(false)
        }
    }
}

pub fn init_usb2_bare_pullup_handoff() -> bool {
    unsafe { init_usb2_bare_pullup_handoff_inner(true) }
}

/// TEMP(flow-map): host-visible code-point blip for the `always` self-test.
/// Toggles the physical pull-up for a short window so the host kernel log
/// timestamps the exact code point that executed. Parks and reset channels
/// are unusable while the unidentified ~17 s biter overrides them; the blip
/// completes in ~0.2 s and restores the previous Run/Stop state.
#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
unsafe fn gate_flow_blip() {
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") != Some("always") {
        return;
    }
    unsafe {
        let running = read(DCTL) & DCTL_RUN_STOP != 0;
        if running {
            write_dctl_safe(read(DCTL) & !DCTL_RUN_STOP);
        } else {
            let dctl = run_stop_value(read(DCTL), read(GSNPSID));
            write(DCTL, dctl | DCTL_RUN_STOP);
        }
        crate::timer::delay_ms(200);
        if running {
            let dctl = run_stop_value(read(DCTL), read(GSNPSID));
            write(DCTL, dctl | DCTL_RUN_STOP);
        } else {
            write_dctl_safe(read(DCTL) & !DCTL_RUN_STOP);
        }
    }
}

#[inline(always)]
unsafe fn set_direct_usb2_vbus_override() {
    // qpr1's dwc3_override_vbus_status(true) updates only
    // HS_PHY_CTRL.UTMI_OTG_VBUS_VALID (bit 20).  The old direct handoff
    // also ORed in SW_SESSVLD_SEL (bit 28), which is an inherited-session
    // override rather than part of the qpr1 peripheral-start contract.
    // When the source-vbus-only A/B is selected, clear that inherited bit
    // explicitly: qscratch_set() is an OR helper and cannot remove it.
    if cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_source_vbus_only) {
        let mut value = read_qscratch(QSCRATCH_HS_PHY_CTRL);
        value &= !(1 << 28);
        value |= 1 << 20;
        write_qscratch(QSCRATCH_HS_PHY_CTRL, value);
        let _ = read_qscratch(QSCRATCH_HS_PHY_CTRL);
    } else {
        qscratch_set(QSCRATCH_HS_PHY_CTRL, (1 << 20) | (1 << 28));
    }
}


/// Reuse the physical USB2 handoff, then add the minimum DWC3 gadget state
/// needed to answer USB control transfers. The PHY and Qualcomm session
/// remain untouched; this is the early Bramble handoff path and is also
/// usable as a standalone probe.
pub fn init_usb2_gadget_handoff() -> bool {
    unsafe {
        // OUTER CALIBRATION, deliberately placed *outside* the handoff entry point.
        // Every other `usb2-live-*` readout lives inside
        // `init_usb2_gadget_reuse_fastboot_ep0`, so if that function is never called
        // its readout cannot run - which makes a zero-pulse reading there circular
        // evidence. This one sits in the caller, which must run for any handoff at
        // all, and issues ONE unconditional pulse.
        //
        //   2 attach lines => the channel works from here, so a zero-pulse reading
        //                     inside the entry point really does mean "not entered"
        //   1 attach line  => nothing is published even here; the channel is dead on
        //                     this route and every pulse reading this session is void
        if option_env!("FULLERENE_USB_UTMI_POSTRUN_READOUT") == Some("usb2-live-outer-calibration") {
            // stop_ms = 500 to match `usb2-live-soffn-count` exactly. The SOFFN
            // sequence is the only one on this channel that ever produced extra
            // attach lines, and it used 500; a 300 ms variant produced none, which
            // may simply have been too short for the host to re-log the attach.
            // Settle FIRST - a pulse issued before the wait produces no host line
            // (measured). This is the outermost probe in the handoff, so it answers
            // whether `init_usb2_gadget_handoff` is reached at all, which nothing
            // inside the entry point can answer.
            readout_keepalive_delay_ms(500);
            ccs_pulse(500);
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_super_speed)]
        // Keep the CLI's --no-core-reset differential effective for the
        // SuperSpeed handoff too.  Before this propagation the SS entry point
        // always passed `reset_core=true`, so run reports claiming to skip
        // CSFTRST were not testing that condition at all.
        return super_speed::init_with_super_speed(
            true,
            !cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_core),
            false,
        );

        #[cfg(all(
            fullerene_aarch64_usb_gadget_handoff_probe,
            not(fullerene_aarch64_usb_gadget_handoff_super_speed)
        ))]
        return gadget_handoff::init_usb2_gadget_reuse_fastboot_ep0();

        // The bare probe is the proven physical baseline on Bramble. Start
        // the gadget diagnostic from that exact pull-up sequence, then add
        // EP0 state on top of it. This makes a failure in the gadget setup
        // observable instead of hiding the already-working link behind a
        // second, subtly different pre-connect sequence.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if !init_usb2_bare_pullup_handoff_inner(true) {
            return false;
        }
        trace_event(TRACE_INIT, 0, 0, 0, 0, 0);
        let snpsid = read(GSNPSID);
        trace_event(TRACE_INIT, 0, 0, 0, 0, snpsid);
        // Keep the Qualcomm session valid while the DWC3 device state is
        // rebuilt. The physical handoff above is deliberately first so the
        // probe preserves the working Bramble reconnect contract; the soft
        // reset below then clears the old Fastboot endpoint state before the
        // complete gadget is connected again.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24); // LANE0_PWR_PRESENT
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        qscratch_set(QSCRATCH_CGCTL, 0x18);
        let gctl = read(GCTL);
        write(
            GCTL,
            (gctl & !GCTL_PRTCAPDIR_MASK) | GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG,
        );

        // Fastboot leaves the DWC3 device controller running while its host
        // endpoint is torn down. After the proven PHY/session preparation,
        // follow Linux's soft-connect order and reset the device state before
        // issuing endpoint commands. The gadget probe intentionally omits
        // stop_running_device(): the bare preparation already cleared
        // Run/Stop and that extra ownership transition was the earlier
        // pre-pull-up failure point.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if !device_soft_reset() {
            log_puts("usb gadget handoff: DWC3 device reset failed\n");
            return false;
        }
        configure_dwc3_global_control();
        #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
        if !stop_running_device() || !device_soft_reset() {
            log_puts("usb gadget handoff: DWC3 reset failed\n");
            return false;
        }
        configure_dwc3_global_control();

        // The fallback also performs a DWC3 reset, so it must receive the
        // same post-reset UTMI/ref-clock programming as the normal handoff
        // path. The earlier bare pull-up sequence may have selected UTMI,
        // but CSFTRST invalidates that controller-side mux state.
        select_utmi_pipe_clock();
        update_dwc3_ref_clock();

        // The bootloader can leave the USB2 core in suspend/LPM state even
        // though the Type-C session is valid. Reapply only the
        // controller-side wakeup bits; do not reset the PHY or clocks.
        qscratch_set(QSCRATCH_GENERAL_CFG, QSCRATCH_GENERAL_CFG_XHCI_REV);
        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        mark_g2w_site(1013);
        write(GUSB2PHYCFG0, usb2);
        let mut usb3 = read(GUSB3PIPECTL0);
        usb3 |= GUSB3PIPECTL_SUSPHY;
        write(GUSB3PIPECTL0, usb3);

        // DWC3 has been stopped/reset above, so the fallback may establish
        // the same DMA ownership boundary as the normal path. This is
        // essential when Fastboot's stream mapping covered only its own
        // buffers and not the Fullerene linker-reserved DMA section.
        if configure_dwc3_smmu() {
            log_puts("usb gadget handoff: DWC3 SMMU DMA-pool map ready\n");
        } else {
            log_puts("usb gadget handoff: DWC3 SMMU DMA-pool map unavailable\n");
            return false;
        }

        // The linker-reserved region is identity-mapped by the early AArch64
        // MMU path. Clean it for the same handoff ordering whether this entry
        // is reached from the standalone probe or from the normal kernel.
        let event_address = ep0_event_address();
        cache_clean(ep0_event_dma_base(), ep0_event_size());
        write(GEVNTADRLO0, event_address as u32);
        write(GEVNTADRHI0, (event_address >> 32) as u32);
        write(GEVNTSIZ0, ep0_event_size() as u32);
        acknowledge_ep0_event_count();
        trace_event(
            TRACE_EVENT_RING_READY,
            event_address as u32,
            (event_address >> 32) as u32,
            ep0_event_size() as u32,
            0,
            0,
        );
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_direct) && !configure_gsi_event_buffers() {
            log_puts("usb: Qualcomm GSI event buffers unavailable\n");
        }
        EVENT_OFFSET = 0;
        GSI_EVENT_OFFSETS = [0; 3];
        GSI_PENDING = [false; 3];
        GSI_CHANNEL_ENDPOINT = [0; 3];
        GSI_CHANNEL_READY = [false; 3];
        GSI_REQUEST_SLOTS = [usize::MAX; 3];
        GSI_RING_BASES = [0; 3];
        GSI_RING_TRB_COUNTS = [0; 3];
        GSI_BUFFER_BASES = [0; 3];
        GSI_BUFFER_LENGTHS = [0; 3];
        GSI_DOORBELL_BASES = [0; 3];
        GSI_RESOURCE_INDEX = [0; 3];
        GSI_RING_ACTIVE = [false; 3];
        RESUME_PENDING = false;
        USB_IN_P3 = false;
        GadgetDriver::reset(gadget_mut());
        udc_mut().reset();
        EP0_STATE = Ep0State::Setup;
        CONFIGURED = false;
        DATA_ENDPOINTS_READY = false;
        DATA_REQUEST_SLOTS = [usize::MAX; 2];
        DATA_RESOURCE_INDEX = [0; 2];
        GSI_GADGET_BOUND = false;
        FUNCTION_BOUND = false;
        ENDPOINTS_READY = false;

        write(
            DCFG,
            if cfg!(fullerene_aarch64_usb_dcfg_lowspeed) {
                DCFG_LOWSPEED
            } else if cfg!(fullerene_aarch64_usb_dcfg_fullspeed) {
                DCFG_FULLSPEED
            } else {
                DCFG_HIGHSPEED
            },
        );
        configure_gadget_start_defaults();
        write(DALEPENA, 0);
        write(DEVTEN, direct_gadget_devten());

        // Drain any power event latched by the Fastboot teardown BEFORE the
        // endpoint commands: a pending PWR event keeps the core's clock/RAM
        // domain gated on this glue, which shows up as SETEPCONFIG /
        // STARTTRANSFER failing or wedging. The full handoff path calls
        // enable_power_events() and its poll loop clears the status; the
        // fallback must do the same synchronously.
        enable_power_events();
        service_power_event();

        // DWC3's device-start contract is: reserve the endpoint resources,
        // configure both directions of EP0, queue the first SETUP TRB, then
        // assert Run/Stop. Without this sequence the PHY can advertise a
        // USB2 pull-up while every host descriptor request times out at EP0.
        if !send_ep_command(0, DEPCMD_DEPSTARTCFG, 0, 0, 0)
            || !configure_endpoint(0, 64, false)
            || !configure_endpoint(1, 64, false)
        {
            log_puts("usb gadget handoff: EP0 configuration failed\n");
            return false;
        }
        ENDPOINTS_READY = true;
        let _ = udc_mut().configure_endpoint(0, 64, false);
        let _ = udc_mut().configure_endpoint(1, 64, false);
        write(DALEPENA, 0b11);
        if !start_setup() {
            log_puts("usb gadget handoff: SETUP STARTTRANSFER failed\n");
            return false;
        }
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_direct)
            || cfg!(fullerene_aarch64_usb_probe_irq_controller)
        {
            enable_gadget_controller_irq();
        }
        // Mirror Linux's post-ep0_out_start IRQ window before connecting the
        // device. In this early probe the equivalent is a bounded synchronous
        // event-ring drain; platform service remains outside this boundary.
        poll_ep0_event_ring();

        // Connect only after the event ring, transfer resources, EP0
        // descriptors, and first SETUP TRB are ready. This produces a fresh
        // USB2 attach without exposing an EP0-less device to the host.
        // Reassert the Qualcomm VBUS/session vote immediately before the
        // final Run/Stop write; this is the glue driver's pre_run_stop hook.
        configure_gadget_speed(cfg!(fullerene_aarch64_usb_dcfg_superspeed));
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        #[cfg(fullerene_aarch64_usb_gadget_handoff_start_defaults_at_runstop)]
        {
            // Linux's __dwc3_gadget_start() applies these controller-wide
            // defaults at the final gadget start boundary. The ordinary
            // handoff applies them before endpoint commands; replay them
            // here as a narrow ordering A/B without rebuilding EP0.
            log_puts("usb gadget handoff: replaying gadget start defaults at Run/Stop\n");
            configure_gadget_start_defaults();
        }
        if !run_stop_device(true) {
            log_puts("usb gadget handoff: DWC3 RUN/STOP timeout\n");
            #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
            return gadget_handoff_fail(7); // Run/Stop
            #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
            return false;
        }

        log_puts("usb gadget handoff: EP0 running\n");
        true
    }
}

/// Host-visible progress beacon for the direct path's reset section: toggle
/// the QSCRATCH session pull-up for 500 ms at each boundary. Every
/// drop/restore pair shows up as one host-side disconnect/re-attach pair in
/// the kernel log, so the LAST beacon visible in the log names exactly how
/// far the code got - including when the code then hangs on a faulting MMIO
/// access, where the watchdog return timing alone cannot localize the stop.
/// Only active in single-attempt (gate readout) runs.
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
unsafe fn init_beacon() {
    unsafe {
        if option_env!("FULLERENE_USB_PROBE_SINGLE_ATTEMPT") != Some("1") {
            return;
        }
        let gate = option_env!("FULLERENE_USB_SIGNAL_CMD_GATE");
        if gate == Some("stall-map") {
            stall_map_beacon();
            return;
        }
        // Cmd-gate runs read their one bit from the return timing and must
        // evaluate inside the pre-bite window; the beacons add ~1 s each to
        // init and pushed the evaluation into the ~17 s watchdog bite.
        if gate.is_some() {
            return;
        }
        ep0_signal_drop_pullup();
        super::timer::delay_ms(500);
        ep0_signal_restore_pullup();
    }
}

/// stall-map beacon: one host-visible DWC3 Run/Stop disconnect/re-attach pair.
/// qpr1's DWC3 source uses DCTL.RUN_STOP for gadget pull-up control; it does
/// not define the DWC2 SDIS bit at DCTL bit 0. The beacon is honored only
/// while the core is running with the link ON, so beacons before Run/Stop are
/// silent by design.
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
unsafe fn stall_map_beacon() {
    unsafe {
        runstop_blips_link_on(1);
    }
}

/// Put the Apps-SMMU into the verified physical=IOVA state used by the
/// direct Bramble DMA differential. The USB2 probe and the SuperSpeed
/// fallback have separate handoff entry points, so keep this ownership
/// transition in one helper; otherwise `--smmu-disable` can silently apply to
/// only one of them.
unsafe fn prepare_smmu_dma_bypass() -> bool {
    #[cfg(fullerene_aarch64_usb_smmu_disable)]
    {
        let scr0 = read_volatile(smmu_reg(SMMU_GR0_SCR0));
        if scr0 == u32::MAX {
            trace_marker(TRACE_PROBE_WATCHDOG, 0x5344_5242); // "SDRB"
            log_puts("usb: SMMU SCR0 unreadable; cannot disable\n");
            return false;
        }
        // sCR0.SMMUEN (bit 0) off, sCR0.CLIENTPD (bit 1) set, and
        // sCR0.WACFG (bits 7:6) = 00 (unattributed transactions pass).
        let new_scr0 = (scr0 & !0x1 & !(0b11 << 6)) | 0x2;
        write_volatile(smmu_reg(SMMU_GR0_SCR0), new_scr0);
        core::arch::asm!("dsb sy", options(nostack));
        let readback = read_volatile(smmu_reg(SMMU_GR0_SCR0));
        let ok = readback == new_scr0;
        trace_event(
            TRACE_SMMU_HANDOFF,
            0x5344_4953,
            scr0,
            new_scr0,
            readback,
            ok as u32,
        );
        if !ok {
            trace_marker(TRACE_PROBE_WATCHDOG, 0x5344_524A); // "SDRJ"
            log_puts("usb: SMMU disable rejected; suppressing pull-up\n");
            return false;
        }
    }
    true
}

/// Reassert the Qualcomm USB30 controller domain without resetting DWC3 or
/// replaying QMP state.  This is kept as one operation so the pre-Run/Stop
/// and post-Run/Stop A/B use exactly the same CX/bus/GDSC/RCG/branch order.
unsafe fn reassert_ss_controller_domain() -> bool {
    unsafe {
        let cx_vote = super::platform::bramble::apply_usb_cx_vote(
            super::platform::bramble::UsbBusVote::Nominal,
        );
        let bus_vote = super::platform::bramble::apply_usb_bus_vote(
            super::platform::bramble::UsbBusVote::Nominal,
        );
        let gdsc = super::platform::bramble::force_enable_usb30_gdsc();
        let sources = super::platform::bramble::configure_usb_controller_clocks(
            super::platform::bramble::UsbBusVote::Nominal,
        );
        let branches = super::platform::bramble::rearm_usb_controller_clock_branches();
        super::platform::bramble::apply_usb_pm_qos(super::platform::bramble::UsbBusVote::Nominal);
        log_hex(
            "usb: SS controller clock reassert mask=",
            u64::from(
                (cx_vote as u8)
                    | ((bus_vote as u8) << 1)
                    | ((gdsc as u8) << 2)
                    | ((sources as u8) << 3)
                    | ((branches as u8) << 4),
            ),
        );
        if !(cx_vote && bus_vote && gdsc && sources && branches) {
            log_puts("usb: SS controller clock reassert incomplete\n");
        }
        crate::timer::delay_us(1_000);
        cx_vote && bus_vote && gdsc && sources && branches
    }
}

/// Keep the USB2 controller domain at the active Android vote while the
/// early image owns a live direct handoff.  This is deliberately narrower
/// than the SuperSpeed reassert path: it does not retune DWC3 clocks, reset
/// the controller, or touch QMP state.  The source-backed contract also
/// re-arms the USB2 controller branches in Android's iface/core/sleep/utmi
/// order, with the HS-PHY reference clock included because the direct
/// handoff preserves Fastboot's PHY ownership.  No endpoint command or TRB
/// mutation is performed here.
#[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_runtime_power_keepalive)]
unsafe fn service_usb2_runtime_power_keepalive() {
    unsafe {
        // Only "no Run/Stop has happened yet" is a real precondition: before the
        // handoff there is nothing to keep alive.
        //
        // The previous guard also bailed out when the core was *stopped*
        // (`DCTL.RUN_STOP == 0`) or when `QMP_PHY_READY` was set. That is backwards
        // for a keepalive: a stopped controller is exactly when RPMh is most likely
        // to have collapsed the domain, and the body below only re-votes the rails,
        // the GDSC and the clock branches - the function's own contract says it
        // "does not retune DWC3 clocks, reset the controller, or touch QMP state".
        // So it is safe with the core halted, and with the old guard the keepalive
        // could be skipped for the whole window in which it was needed.
        if RUN_STOP_TICK == 0 {
            return;
        }
        let frequency = arch_counter_frequency();
        if frequency == 0 {
            return;
        }
        let now = arch_counter();
        if USB2_RUNTIME_KEEPALIVE_NEXT != 0 && now < USB2_RUNTIME_KEEPALIVE_NEXT {
            return;
        }

        let votes = super::platform::bramble::refresh_usb_domain_votes(
            super::platform::bramble::UsbBusVote::Nominal,
            false,
        );
        let gdsc = super::platform::bramble::force_enable_usb30_gdsc();
        let branches = super::platform::bramble::rearm_usb2_android_clock_branches();
        let utmi = super::platform::bramble::enable_usb2_utmi_clock();
        let ref_clock = super::platform::bramble::enable_usb_hs_phy_ref_clock();
        super::platform::bramble::apply_usb_pm_qos(super::platform::bramble::UsbBusVote::Nominal);
        USB2_RUNTIME_KEEPALIVE_COUNT = USB2_RUNTIME_KEEPALIVE_COUNT.wrapping_add(1);
        // Retain a sparse proof of the active-domain refresh without filling
        // the ring during a long stable session.  The first refresh and each
        // 32nd refresh carry the outcome bits; the host can correlate the
        // marker with the descriptor timeout window in Tshark.
        if USB2_RUNTIME_KEEPALIVE_COUNT == 1 || USB2_RUNTIME_KEEPALIVE_COUNT & 31 == 0 {
            trace_event(
                TRACE_USB_DEVICE_ERROR,
                0x4b41_4c56, // "KALV"
                votes as u32,
                gdsc as u32,
                ref_clock as u32,
                (branches as u32) | ((utmi as u32) << 1) | (USB2_RUNTIME_KEEPALIVE_COUNT << 8),
            );
        }
        USB2_RUNTIME_KEEPALIVE_NEXT = now.saturating_add(frequency.saturating_div(2).max(1));
    }
}

#[cfg(not(fullerene_aarch64_usb_gadget_handoff_usb2_runtime_power_keepalive))]
#[inline]
unsafe fn service_usb2_runtime_power_keepalive() {}


/// Consume the DWC3 EP0 event ring without touching platform power, Type-C,
/// or SMMU state. Linux has an IRQ window immediately after arming the first
/// SETUP TRB; the early handoff uses this bounded synchronous equivalent before
/// the normal polling path owns the controller.
///
/// DWC3 GEVNTCOUNT is a write-to-consume register. Android msm's
/// `dwc3_event_buffers_setup()` initializes the buffer by writing zero to the
/// complete register, including when the previous Fastboot owner left a stale
/// event-buffer state behind. Factory ABL instead preserves the register's
/// EHB bit during that initial publication; the ABL event-consume A/B carries
/// that source-derived difference together with its per-event ACK order.
unsafe fn acknowledge_ep0_event_count() {
    unsafe {
        let value = if cfg!(fullerene_aarch64_usb_abl_event_consume) {
            read(GEVNTCOUNT0) & GEVNTCOUNT_EHB
        } else {
            0
        };
        write(GEVNTCOUNT0, value);
        core::arch::asm!("dsb sy", options(nostack));
    }
}

/// Re-apply the EP0 event-buffer ownership at the Run/Stop boundary.
///
/// Android's msm-4.19 `dwc3_gadget_run_stop(true)` performs event-buffer
/// setup immediately before restarting the gadget. The direct handoff
/// normally publishes the ring earlier, after stopping Fastboot. Keep this
/// diagnostic deliberately narrow: it re-writes only the EP0 event address,
/// size, and consumed count, without clearing endpoint state or touching the
/// physical pull-up.
#[cfg(any(
    fullerene_aarch64_usb_gadget_handoff_event_ring_at_runstop,
    fullerene_aarch64_usb_gadget_handoff_gadget_restart_at_runstop,
    fullerene_aarch64_usb_gadget_handoff_ss_core_reset_at_runstop,
    fullerene_aarch64_usb_usb2_core_reset_at_runstop
))]
unsafe fn republish_ep0_event_ring_at_runstop() {
    let event_address = unsafe { ep0_event_address() };
    let event_size = unsafe { ep0_event_size() };
    unsafe {
        // The controller is still halted here, so invalidate CPU cache lines
        // before handing the cacheable ring back to DWC3. Cleaning stale CPU
        // lines could overwrite an event that the controller posted earlier.
        cache_invalidate(ep0_event_dma_base(), event_size);
        write(GEVNTADRLO0, event_address as u32);
        write(GEVNTADRHI0, (event_address >> 32) as u32);
        write(GEVNTSIZ0, event_size as u32);
        acknowledge_ep0_event_count();
        EVENT_OFFSET = 0;
        core::arch::asm!("dsb sy", options(nostack));
        trace_event(
            TRACE_EVENT_RING_READY,
            0x5253_544f, // "RSTO"
            event_address as u32,
            (event_address >> 32) as u32,
            event_size as u32,
            read(DSTS),
        );
    }
}

/// Re-run the Android msm `__dwc3_gadget_start()` EP0 portion immediately
/// before Run/Stop. Android's `dwc3_gadget_run_stop(true)` uses this restart
/// boundary after event-buffer setup, while the direct handoff normally
/// configured EP0 only once after taking over from Fastboot.
///
/// This diagnostic keeps the known direct-path platform state and reapplies
/// the selected speed after any device-core soft reset. It
/// only repeats the DWC3 gadget-start command sequence: open a resource
/// window, allocate endpoint resources, initialize both physical EP0
/// directions, and arm the initial SETUP TRB. The caller deliberately
/// continues to the final Run/Stop transition when this best-effort restart
/// is incomplete, matching Android's void `dwc3_gadget_restart()` contract.
#[cfg(any(
    fullerene_aarch64_usb_gadget_handoff_gadget_restart_at_runstop,
    fullerene_aarch64_usb_gadget_handoff_ss_core_reset_at_runstop,
    fullerene_aarch64_usb_usb2_core_reset_at_runstop
))]
unsafe fn restart_gadget_at_runstop(super_speed: bool) -> bool {
    unsafe {
        #[cfg(any(
            fullerene_aarch64_usb_gadget_handoff_ss_core_reset_at_runstop,
            fullerene_aarch64_usb_usb2_core_reset_at_runstop
        ))]
        {
            // A device-core soft reset can restore GCTL.PRTCAPDIR to its
            // reset value. Android selects DEVICE before the reset, but this
            // helper runs after it, so repeat the source-order device-mode
            // tail before publishing gadget state.
            configure_dwc3_device_mode();
            if super_speed {
                configure_usb31_lfps_exit_timer();
            }
        }
        republish_ep0_event_ring_at_runstop();
        // The upstream `dwc3_gadget_run_stop(true)` calls
        // `__dwc3_gadget_start()` and then rewrites DCFG.SPEED immediately
        // before asserting Run/Stop. Keep the existing restart A/B's earlier
        // placement by default, but move it after the full gadget-start
        // sequence in the source-order control.
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_gadget_start_only_at_runstop) {
            configure_gadget_speed(super_speed);
        }
        configure_gadget_start_defaults();
        #[cfg(fullerene_aarch64_usb_gadget_handoff_start_defaults_at_runstop)]
        {
            // On DWC_usb31 qpr1's __dwc3_gadget_start() also reapplies the
            // revision-gated GUCTL/GSBUSCFG1 gadget-start deltas.  When the
            // direct USB2 A/B performs device-core reset at this boundary,
            // restore that source-owned state in the same reset-after-start
            // epoch instead of carrying the pre-reset register values into
            // the final endpoint commands.
            apply_usb31_gadget_reference_deltas();
        }
        write(DALEPENA, 0);
        ENDPOINTS_READY = false;
        EP0_SETUP_ARMED = false;
        PENDING_SETUP_ARM = false;
        EP0_RESOURCE_INDEX = [0; 2];
        EP0_STATE = Ep0State::Setup;

        if !send_ep_command(0, DEPCMD_DEPSTARTCFG, 0, 0, 0) {
            trace_event(TRACE_SETUP_QUEUED, 0x5253_4641, 0, 0, 0, read(DSTS)); // "RSFA"
            return false;
        }
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_no_transfer_resource) {
            // qpr1's start_config() assigns one resource to every endpoint
            // advertised by GHWPARAMS3 before either EP0 SETEPCONFIG command.
            for endpoint in 0..qpr1_endpoint_count() {
                if !set_transfer_resource(endpoint) {
                    trace_event(
                        TRACE_SETUP_QUEUED,
                        0x5253_5253, // "RSRS"
                        endpoint as u32,
                        0,
                        0,
                        read(DSTS),
                    );
                    return false;
                }
            }
        }

        // Android starts with the SuperSpeed EP0 descriptor and changes it
        // to 64 bytes from Connect Done for a USB2 link. When this helper is
        // used for the USB2-only handoff, Connect Done is the first host
        // boundary and may not reach the software event consumer before the
        // first SETUP. Keep the existing A/B flag consistent across both
        // endpoint-construction epochs: the explicit 512-byte experiment
        // retains the source initial state, while the baseline USB2 restart
        // publishes the post-Connect-Done 64-byte state directly.
        let restart_ep0_packet_size = if cfg!(fullerene_aarch64_usb_ep0_initial_512) {
            INITIAL_EP0_MAX_PACKET_SIZE
        } else {
            64
        };
        let epcfg0 =
            configure_endpoint_config(0, restart_ep0_packet_size, DEPCFG_EP_TYPE_CONTROL, false, 0);
        if !epcfg0 {
            trace_event(
                TRACE_SETUP_QUEUED,
                0x52534546, // "RSEF"
                epcfg0 as u32,
                0,
                0,
                read(DSTS),
            );
            return false;
        }
        // qpr1's __dwc3_gadget_ep_enable() publishes each endpoint's
        // DALEPENA bit immediately after its SETEPCONFIG command. Keep the
        // same ownership boundary instead of exposing both directions only
        // after EP0-IN configuration has completed.
        write(DALEPENA, read(DALEPENA) | (1 << 0));
        trace::live_dalepena_config(0, read(DALEPENA));
        let epcfg1 =
            configure_endpoint_config(1, restart_ep0_packet_size, DEPCFG_EP_TYPE_CONTROL, false, 0);
        if !epcfg1 {
            trace_event(
                TRACE_SETUP_QUEUED,
                0x52534546, // "RSEF"
                epcfg0 as u32,
                epcfg1 as u32,
                0,
                read(DSTS),
            );
            return false;
        }
        write(DALEPENA, read(DALEPENA) | (1 << 1));
        trace::live_dalepena_config(1, read(DALEPENA));
        ENDPOINTS_READY = true;
        let _ = udc_mut().configure_endpoint(0, restart_ep0_packet_size as u16, false);
        let _ = udc_mut().configure_endpoint(1, restart_ep0_packet_size as u16, false);
        // The canonical qpr1 path arms CONTROL_SETUP before Run/Stop. The
        // Bramble timing A/B can deliberately defer only this STARTTRANSFER
        // while retaining the Android endpoint/resource restart; the common
        // post-Run/Stop arm window then uses the same U0-guarded retry as the
        // normal direct handoff.
        let defer_setup = cfg!(fullerene_aarch64_usb_gadget_handoff_start_after_connect);
        let armed = if defer_setup {
            PENDING_SETUP_ARM = true;
            false
        } else {
            start_setup()
        };
        trace_event(
            TRACE_SETUP_QUEUED,
            0x52534152, // "RSAR"
            armed as u32,
            0,
            8,
            read(DSTS),
        );
        if armed {
            poll_ep0_event_ring();
        }
        // `__dwc3_gadget_start()` enables the controller event mask after
        // arming EP0. The direct path polls the same ring, but a core reset at
        // this boundary still clears DEVTEN, so publish the selected qpr1 or
        // broad controller-event set before Run/Stop.
        write(DEVTEN, direct_gadget_devten());
        if cfg!(any(
            fullerene_aarch64_usb_gadget_handoff_gadget_start_only_at_runstop,
            fullerene_aarch64_usb_gadget_handoff_gadget_speed_after_restart
        )) {
            configure_gadget_speed(super_speed);
        }
        armed
    }
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

/// Update the signal-probe latches. Called from `ep0_signal_code()` so a
/// polling-only consumer does not need an extra tracing channel.
unsafe fn update_signal_latches() {
    unsafe {
        // In start-after-connect mode the initial TRB is intentionally not
        // armed until the host link is usable.  Before that point the setup
        // buffer aliases an unowned TRB slot, so its reset/firmware contents
        // are not evidence of a host SETUP packet.  Keep every TRB/buffer
        // latch behind the same ownership bit used by the arm guard.
        let setup_armed = EP0_SETUP_ARMED;
        // The core retires a TRB by clearing HWO over DMA. Invalidate the
        // cached line first so the CPU observes the controller's write.
        let trb = ep0_trb_ptr(0);
        let host_reset_seen = SIGNAL_USB_RESET_SEEN;
        if setup_armed && host_reset_seen {
            cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
            if read_volatile(addr_of!((*trb).ctrl)) & TRB_HWO == 0 {
                SIGNAL_SETUP_TRB_RETIRED = true;
            }
        }
        let setup = ep0_setup_data_ptr() as *const u8;
        if setup_armed && host_reset_seen {
            cache_invalidate(setup as usize, 8);
        }
        if setup_armed && host_reset_seen && setup == trb.cast::<u8>() {
            // qpr1 deliberately aliases the eight-byte SETUP payload to the
            // first EP0 TRB. Before a host packet arrives, the first two
            // words therefore still contain the TRB's DMA destination. A
            // simple "any non-zero byte" test would report a false SETUP on
            // every armed transfer; compare the aliased words with the
            // expected DMA address instead. A real GET_DESCRIPTOR SETUP
            // overwrites them with the packet bytes.
            let expected = dma_iova_for(setup as usize);
            let current_low = read_volatile(setup.cast::<u32>());
            let current_high = read_volatile(setup.add(4).cast::<u32>());
            if current_low != expected as u32 || current_high != (expected >> 32) as u32 {
                SIGNAL_SETUP_PACKET_RECEIVED = true;
            }
        } else if setup_armed && host_reset_seen {
            // The separate-buffer A/B is zeroed before arming, so any
            // non-zero byte is a valid indication that the controller DMAed
            // a SETUP packet into it.
            for offset in 0..8 {
                if read_volatile(setup.add(offset)) != 0 {
                    SIGNAL_SETUP_PACKET_RECEIVED = true;
                    break;
                }
            }
        }
        // DSTS_HIGHSPEED is zero, so the link state cannot be read from
        // ConnectSpd. A changing SOF frame number instead proves the core is
        // receiving packets from the host at the transaction level.
        if host_reset_seen {
            let sofn = ((read(DSTS) & (0x3fff << 3)) >> 3) as u16;
            if !SIGNAL_SOF_BASELINED {
                SIGNAL_LAST_SOFFN = sofn;
                SIGNAL_SOF_BASELINED = true;
            } else if sofn != SIGNAL_LAST_SOFFN {
                SIGNAL_LAST_SOFFN = sofn;
                SIGNAL_SOF_SEEN = true;
            }
            // Latch the core's view of the USB2 link for the link-state ladder.
            let dsts = read(DSTS);
            match (dsts >> 18) & 0xf {
                0 => SIGNAL_LNKST_U0 = true,      // ON: link up at the detected speed
                5 => SIGNAL_LNKST_RXDET = true,   // RX.DETECT: core still waiting
                7 => SIGNAL_LNKST_POLLING = true, // POLLING: chirp phase observed
                14 => SIGNAL_LNKST_RESET = true,  // RESET: bus reset observed
                _ => {}
            }
            if dsts & DSTS_DEVCTRLHLT != 0 || read(DCTL) & DCTL_RUN_STOP == 0 {
                // A halted core or a cleared Run/Stop after a verified start makes
                // the physical attach a QSCRATCH session-override phantom.
                SIGNAL_CORE_HALTED = true;
            }
        }
    }
}

/// Encode the polled EP0/DMA observables as one host-visible code. The probe
/// drops the physical pull-up `3 * code` seconds after attach, so the host
/// dmesg delta between "new high-speed USB device" and "USB disconnect" names
/// the first stage that provably worked:
///   1 = a host USB Reset event reached the event-ring consumer
///   2 = DWC3 retired the armed EP0 SETUP TRB (HWO cleared over DMA)
///   3 = the SETUP packet payload was DMAed into the setup buffer
///   5 = SOF frames are arriving (transaction-level RX alive)
///   0 = none of the above (no drop; the host only sees its own -110)
/// SMMU read-only probe codes are handled by `probe_smmu_stream_state()`.
pub fn ep0_signal_code() -> u32 {
    unsafe {
        update_signal_latches();
        if SIGNAL_EVENT_DELIVERED && SIGNAL_USB_RESET_SEEN {
            return 1;
        }
        if SIGNAL_SETUP_TRB_RETIRED {
            return 2;
        }
        if SIGNAL_SETUP_PACKET_RECEIVED {
            return 3;
        }
        if SIGNAL_SOF_SEEN {
            return 5;
        }
        0
    }
}

/// True once the EP0 SETUP payload has been observed either by the retained
/// setup latch or by the DMA-buffer sampler. The signal probe uses this as a
/// precise pre-response boundary: it is intentionally earlier than any
/// descriptor DATA/TRB completion and does not infer a wire-level CRC error.
pub fn ep0_setup_packet_seen() -> bool {
    unsafe { SIGNAL_SETUP_PACKET_RECEIVED }
}

/// True once the polling path has consumed a non-empty DWC3 device/event-ring
/// record. This is deliberately one boundary earlier than SETUP parsing and
/// lets the signal probe test event ownership without changing PHY or TRB
/// configuration.
pub fn ep0_event_delivered() -> bool {
    unsafe { SIGNAL_EVENT_DELIVERED }
}

/// Compact same-boot progress mask for the read-only diagnostic channel:
/// bit 0 = a DWC3 event reached the software consumer, bit 1 = the armed
/// EP0 SETUP TRB retired, bit 2 = a non-zero SETUP payload was DMAed, and
/// bit 3 = the DSTS SOF frame changed while polling. This combines latches
/// from the whole descriptor window so one selector can distinguish
/// link-up-without-RX from RX-without-event-DMA.
pub fn ep0_progress_mask() -> u32 {
    unsafe {
        update_signal_latches();
        u32::from(SIGNAL_EVENT_DELIVERED)
            | (u32::from(SIGNAL_SETUP_TRB_RETIRED) << 1)
            | (u32::from(SIGNAL_SETUP_PACKET_RECEIVED) << 2)
            | (u32::from(SIGNAL_SOF_SEEN) << 3)
    }
}

/// True once the DWC3 device-event stream delivered an ERRATIC_ERROR,
/// CMD_COMPLETE, or OVERFLOW notification. The signal probe uses this to
/// separate a controller-reported device error from the host's generic
/// xHCI `-EPROTO` completion.
pub fn dwc3_device_error_seen() -> bool {
    unsafe { SIGNAL_DWC3_DEVICE_ERROR }
}

/// Link-state variant of the signal ladder. Priority reflects the deepest
/// USB2 link state the core ever reported after a verified Run/Stop start:
///   1 = ON (U0): the core believes the link is up at the detected speed
///   2 = core halted itself or Run/Stop read back cleared (phantom attach)
///   3 = RESET: bus reset observed but never ON
///   4 = POLLING: chirp phase observed but never ON
///   5 = RX.DETECT only: the core never saw the host session
///   0 = none of the above
pub fn ep0_link_signal_code() -> u32 {
    unsafe {
        update_signal_latches();
        if SIGNAL_LNKST_U0 {
            return 1;
        }
        if SIGNAL_CORE_HALTED {
            return 2;
        }
        if SIGNAL_LNKST_RESET {
            return 3;
        }
        if SIGNAL_LNKST_POLLING {
            return 4;
        }
        if SIGNAL_LNKST_RXDET {
            return 5;
        }
        0
    }
}

/// Raw DSTS.USBLNKST nibble at poll time. The dedicated raw run drops the
/// pull-up at `3 + 2 * value` seconds, so the host-visible delta names the
/// exact link-state encoding the core reports after its verified start.
pub fn ep0_raw_link_signal_code() -> u32 {
    unsafe {
        update_signal_latches();
        (read(DSTS) >> 18) & 0xf
    }
}

/// One-shot raw link-state readout for the lnk-nib gate: the 4-bit
/// USBLNKST nibble, or 16 when the core reads halted, or 17 when Run/Stop
/// reads back cleared (a QSCRATCH/VBUSVLDEXT0 phantom attach - the PHY can
/// still answer the host's port reset and chirps while the core is out of
/// the loop, which would masquerade as a link-FSM desync).
pub fn ep0_raw_link_nibble() -> u32 {
    unsafe {
        let dsts = read(DSTS);
        if dsts & DSTS_DEVCTRLHLT != 0 {
            return 16;
        }
        if read(DCTL) & DCTL_RUN_STOP == 0 {
            return 17;
        }
        (dsts >> 18) & 0xf
    }
}

/// Heartbeat control: toggle DCTL Run/Stop in one-second intervals starting
/// immediately after the verified connect. If the host still records a full
/// 5-second descriptor timeout against a continuously attached port, the
/// post-attach core ignores DCTL Run/Stop clears and the pull-up cannot be
/// dropped by software at all.
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
fn ep0_signal_heartbeat_check() {
    if option_env!("FULLERENE_USB_SIGNAL_HEARTBEAT") != Some("1") {
        return;
    }
    unsafe {
        for _ in 0..3 {
            let _ = run_stop_device(false);
            super::timer::delay_ms(1000);
            let _ = run_stop_device(true);
            super::timer::delay_ms(1000);
        }
    }
}

/// Control variant of the early drop: run immediately BEFORE the first
/// Run/Stop. If the pull-up still appears with this unconditional drop, the
/// Qualcomm session overrides do not gate the attach at all and the pull-up
/// is purely core-driven (DCTL.TermSelect).
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
fn ep0_signal_pre_runstop_drop_check() {
    if option_env!("FULLERENE_USB_SIGNAL_PRE_DROP") != Some("1") {
        return;
    }
    unsafe {
        trace_marker(TRACE_PROBE_WATCHDOG, 0x5349_5052);
        ep0_signal_drop_pullup();
    }
}

/// One-bit host-visible signal: sample the condition latches for a bounded
/// window right after the first post-connect event poll and permanently drop
/// the pull-up when the requested condition is met. The host then never sees
/// the descriptor timeout (-110), so the ABSENCE of that line is the readout.
///   9 = unconditional (control run: proves the drop mechanism itself)
///   1 = a host USB Reset event reached the event-ring consumer
///   2 = the armed EP0 SETUP TRB was retired (HWO cleared over DMA)
///   3 = the SETUP packet payload was DMAed into the setup buffer
///   5 = SOF frame numbers are changing (transaction-level RX alive)
///   6 = raw DSTS USBLNKST entered RESET, without requiring an event record
///   7 = raw DSTS USBLNKST entered POLLING, without requiring an event record
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
fn ep0_signal_early_drop_check() {
    let condition = match option_env!("FULLERENE_USB_SIGNAL_EARLY_DROP") {
        Some("1") => 1,
        Some("2") => 2,
        Some("3") => 3,
        Some("5") => 5,
        Some("6") => 6,
        Some("7") => 7,
        Some("9") => 9,
        _ => 0,
    };
    if condition == 0 {
        return;
    }
    unsafe {
        let mut observed = 0;
        let mut ms = 0u32;
        // Bramble can take roughly 14 seconds from Fastboot's disconnect to
        // the host's first high-speed attach. The old 1.5-second window ended
        // before any SETUP packet could arrive, so it could not distinguish a
        // dead EP0 from normal pre-attach delay. Keep the safe 20-second
        // default, but honor the harness' explicit observation-window knob so
        // a slower attach can be measured without another code change.
        let observe_ms = option_env!("FULLERENE_USB_PROBE_OBSERVE_SECS")
            .and_then(|value| value.parse::<u64>().ok())
            .and_then(|secs| secs.checked_mul(1_000))
            .map(|millis| millis.min(u32::MAX as u64) as u32)
            .filter(|millis| *millis > 0)
            .unwrap_or(20_000);
        while ms < observe_ms {
            ms += 1;
            if condition != 9 {
                // Consume any pending events first. Condition 1 is narrowed
                // to the host USB Reset event so inherited Fastboot records
                // cannot masquerade as an on-wire boundary.
                poll_ep0_event_ring();
                update_signal_latches();
                // Select the requested latch directly. Event delivery is a
                // prerequisite for the later observations, so a priority
                // ladder here would make code 2/3/5 impossible whenever the
                // reset or Connect Done event had already arrived.
                observed = match condition {
                    1 if SIGNAL_EVENT_DELIVERED && SIGNAL_USB_RESET_SEEN => 1,
                    2 if SIGNAL_SETUP_TRB_RETIRED => 2,
                    3 if SIGNAL_SETUP_PACKET_RECEIVED => 3,
                    5 if SIGNAL_SOF_SEEN => 5,
                    6 if ((read(DSTS) >> 18) & 0xf) == 14 => 6,
                    7 if ((read(DSTS) >> 18) & 0xf) == 7 => 7,
                    _ => 0,
                };
                if observed == condition {
                    break;
                }
            }
            super::timer::delay_ms(1);
        }
        if condition == 9 || observed == condition {
            trace_marker(TRACE_PROBE_WATCHDOG, 0x5349_4544 | (condition << 8));
            ep0_signal_drop_pullup();
        }
    }
}

/// Continue the early-drop diagnostic after the handoff has returned to the
/// direct probe's normal polling owner. The initial bounded window can end
/// before xHCI publishes its first USB2 attach; keep the same condition
/// read-only until the polling loop's ordinary recovery deadline instead of
/// making the pre-return sleep unbounded.
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
pub fn ep0_signal_early_drop_poll() -> bool {
    let condition = match option_env!("FULLERENE_USB_SIGNAL_EARLY_DROP") {
        Some("1") => 1,
        Some("2") => 2,
        Some("3") => 3,
        Some("5") => 5,
        Some("6") => 6,
        Some("7") => 7,
        Some("9") => 9,
        _ => 0,
    };
    if condition == 0 {
        return false;
    }
    unsafe {
        update_signal_latches();
        let observed = match condition {
            1 if SIGNAL_EVENT_DELIVERED && SIGNAL_USB_RESET_SEEN => true,
            2 if SIGNAL_SETUP_TRB_RETIRED => true,
            3 if SIGNAL_SETUP_PACKET_RECEIVED => true,
            5 if SIGNAL_SOF_SEEN => true,
            6 if ((read(DSTS) >> 18) & 0xf) == 14 => true,
            7 if ((read(DSTS) >> 18) & 0xf) == 7 => true,
            9 => true,
            _ => false,
        };
        if observed {
            trace_marker(TRACE_PROBE_WATCHDOG, 0x5349_4550 | (condition << 8));
            ep0_signal_drop_pullup();
            return true;
        }
    }
    false
}

/// True when the diagnostic quiet window (FULLERENE_USB_QUIET_AFTER_SECS)
/// has passed: the probe must stop ALL MMIO access, including the watchdog
/// pet, so a surviving reboot is provably external.
pub fn mmio_quiet_active() -> bool {
    unsafe {
        if let Some(secs) = option_env!("FULLERENE_USB_QUIET_AFTER_SECS")
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
        {
            if RUN_STOP_TICK != 0 {
                let frequency = arch_counter_frequency();
                if frequency != 0 {
                    return arch_counter().saturating_sub(RUN_STOP_TICK)
                        >= frequency.saturating_mul(secs);
                }
            }
        }
        false
    }
}

/// Evaluate the FULLERENE_USB_SIGNAL_CMD_GATE condition against the
/// retained-trace harvest. None = no gate configured (or unparseable).
/// Latch the "core link FSM read U0" fact for the lnk-ever-on gate. Called
/// from poll, so it samples every loop iteration of whichever loop owns the
/// controller.
pub fn link_on_sample() {
    unsafe {
        let dsts = read(DSTS);
        if !LNK_EVER_ON && dsts & DSTS_DEVCTRLHLT == 0 && (dsts >> 18) & 0xf == 0 {
            LNK_EVER_ON = true;
        }
        if !LNK_MID_SEEN {
            let state = (dsts >> 18) & 0xf;
            if state == 8 || state == 9 || state == 11 || state == 14 || state == 15 {
                LNK_MID_SEEN = true;
            }
        }
    }
}

/// "lnk3" gate readout: did any poll sample since probe entry observe the
/// core's link FSM in a mid-transaction state (RECOV=8, HRESET=9, LPBK=11,
/// RESET=14, RESUME=15)? Latched in link_on_sample.
pub fn lnk_mid_transaction_seen() -> bool {
    unsafe { LNK_MID_SEEN }
}

/// Window-end "arm alive" probe for the armalive gate: has ANY EP0 SETUP
/// Start Transfer retired since probe entry, and has the host DMA'd a
/// SETUP into the buffer? Bit 0 = an armed TRB is still pending (a
/// retired arm whose SETUP the core never latched), bit 1 = a host DMA'd
/// SETUP sits in the buffer (a retired arm the core consumed). Both zero
/// = no Start Transfer ever retired in the window (persistent command
/// wedge). The buffer read mirrors the XferComplete path's
/// invalidate+read; the content is never zeroed after consumption, so the
/// read also covers an arm consumed inside the window.
/// Raw core link-FSM state (DSTS.USBLNKST, bits 21:18). This core is a
/// DWC_usb31 (>= 1.94a): upstream v5.10 core.h defines
/// DWC3_DSTS_USBLNKST_MASK as (0x0f << 18) and encodes the link states as
/// U0 = 0x00 (in HS, "ON"), U1 = 0x01, U2 = 0x02 (HS "SLEEP"),
/// U3 = 0x03 (HS "SUSPEND"), SS_DIS = 0x04, RX_DET = 0x05,
/// SS_INACT = 0x06, POLL = 0x07, RECOV = 0x08, HRESET = 0x09,
/// CMPLY = 0x0a, LPBK = 0x0b, RESET = 0x0e, RESUME = 0x0f - the same
/// table the AOSP dwc3 driver on this SoC uses. The legacy shift-18
/// guard reads exactly this field, so it is the correct reference.
/// (The older DWC_usb30 layout, USBLNKST at bits 23:20 with U0 = 1, does
/// NOT apply here.) The lnkalive gate (third pass) bites on the
/// mid-transaction states 8/9/11/14/15, so an early return names a core
/// stuck in the reset/resume handshake at the sample instant; a
/// non-early return names a link-down state (RX_DET/SS_INACT/SS_DIS/
/// POLL/CMPLY).
pub fn dsts_raw_link_state() -> u32 {
    unsafe { (read(DSTS) >> 18) & 0xf }
}

/// Raw link-transaction debug state from GDBGLTSSM. The Qualcomm DWC3
/// glue treats bits 25:22 as LINKSTATE (the same 4-bit selector used by
/// DSTS.USBLNKST, but read directly from the link-layer TX/RX FSM); this
/// distinguishes a reserved/legacy DSTS encoding such as 13 from the
/// physical LTSSM state and its real sub-state bits.
pub fn gdb_ltssm_link_state() -> u32 {
    unsafe {
        let snpsid = read(GSNPSID);
        let offset = if matches!(snpsid >> 16, DWC31_IP | DWC32_IP) {
            DWC31_LINK_GDBGLTSSM
        } else {
            GDBGLTSSM
        };
        (read(offset) >> 22) & 0xf
    }
}

/// DSTS SOF frame number (bits 16:3). A value that changes across samples
/// proves the core is receiving packets from the host at the transaction
/// level even when the link FSM never reports U0 or a mid-transaction
/// state.
/// DSTS.DEVCTRLHLT readout for the haltbit gate.
pub fn dsts_device_ctrl_halted() -> bool {
    unsafe { read(DSTS) & DSTS_DEVCTRLHLT != 0 }
}

/// DCTL.RUN_STOP readout for the dctlbit gate.
pub fn dctl_run_stop_set() -> bool {
    unsafe { read(DCTL) & DCTL_RUN_STOP != 0 }
}

/// One-shot DSTS word snapshot for raw readout gates.
pub fn dsts_word_snapshot() -> u32 {
    unsafe { read(DSTS) }
}

pub fn dsts_sof_frame_number() -> u32 {
    unsafe { (read(DSTS) >> 3) & 0x3fff }
}

/// U0_ARM_STATUS readout for the armstat gate: 0 = the pre-Run/Stop EP0
/// OUT STARTTRANSFER retired cleanly, 8 = it did not retire in the command
/// timeout, and the smaller values name the preceding setup step.
pub fn u0_arm_status_probe() -> u32 {
    unsafe { U0_ARM_STATUS }
}

/// Return the newest retained EP1 STARTTRANSFER command word. The value is
/// the completed DEPCMD register with the controller's status nibble intact
/// (bit 31 set = the command timed out; bit 16 alone = healthy XferRscIdx 1);
/// `0xFFFF_FFFF` means that no EP1 command was captured in this boot's trace.
pub fn ep1_start_status_probe() -> u32 {
    unsafe {
        harvest_trace_outcome();
        TRACE_HARVEST_EP1
    }
}

pub fn armalive_probe() -> u32 {
    unsafe {
        let mut state = 0u32;
        if EP0_SETUP_ARMED {
            state |= 0x1;
        }
        let setup = ep0_setup_data_ptr();
        cache_invalidate(setup as usize, 8);
        for offset in 0..8 {
            if read_volatile(setup.add(offset)) != 0 {
                state |= 0x2;
                break;
            }
        }
        state
    }
}

pub fn cmd_gate_condition_met() -> Option<bool> {
    // "mrad" skips every gate branch and the generic gate so the probe
    // falls through to the tail readout: the composite diag code rides the
    // tail wait (code*700 ms) ahead of the PSCI reset, and the Android
    // return time names the code.
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("mrad") {
        return None;
    }
    let want = option_env!("FULLERENE_USB_SIGNAL_CMD_GATE")?;
    unsafe {
        // Re-harvest against this run's live trace: the gate must evaluate
        // the attempt that just flowed through the observation window, not
        // the init-time harvest of a previous attempt.
        harvest_trace_outcome();
        let ok = match want {
            // Mechanism self-test: unconditionally true. A clean (non-watchdog)
            // readout with this gate proves the gate path and our edits are
            // live in the running image.
            "always" => true,
            "timeout" => TRACE_HARVEST & 0x8000_0000 != 0,
            "done" => TRACE_HARVEST != 0xFFFF_FFFF && TRACE_HARVEST & 0x8000_0000 == 0,
            "last-timeout" => TRACE_HARVEST_LAST & 0x8000_0000 != 0,
            "last-done" => {
                TRACE_HARVEST_LAST != 0xFFFF_FFFF && TRACE_HARVEST_LAST & 0x8000_0000 == 0
            }
            "setup" => TRACE_HARVEST_SETUP > 0,
            "desc" => TRACE_HARVEST_DESC > 0,
            "statusq" => TRACE_HARVEST_STATUSQ > 0,
            "armed" => TRACE_HARVEST_ARMED > 0,
            "arm-first" => {
                TRACE_HARVEST_ARM_SEQ != 0xFFFF_FFFF
                    && TRACE_HARVEST_SETUP_SEQ != 0xFFFF_FFFF
                    && TRACE_HARVEST_ARM_SEQ < TRACE_HARVEST_SETUP_SEQ
            }
            "setup-first" => {
                TRACE_HARVEST_SETUP_SEQ != 0xFFFF_FFFF
                    && (TRACE_HARVEST_ARM_SEQ == 0xFFFF_FFFF
                        || TRACE_HARVEST_ARM_SEQ > TRACE_HARVEST_SETUP_SEQ)
            }
            "connect" => TRACE_HARVEST_CONNECT > 0,
            "addr" => TRACE_HARVEST_ADDR > 0,
            "readall" => TRACE_HARVEST_ADDR2 > 0,
            // Data-phase (EP1 IN) arm outcome gates: TRACE_HARVEST_EP1 holds
            // the newest EP1 STARTTRANSFER raw DEPCMD register (status bits
            // 15:12), or 0xFFFF_FFFF when no EP1 command was ever issued.
            "ep1-none" => TRACE_HARVEST_EP1 == 0xFFFF_FFFF,
            // A timed-out command carries the 0x8000_0000 flag; bit 16 alone
            // is a healthy XferRscIdx=1 completion on physical EP1.
            "ep1-wedge" => {
                TRACE_HARVEST_EP1 != 0xFFFF_FFFF && (TRACE_HARVEST_EP1 & 0x8000_0000) != 0
            }
            // A clean data-phase start: the command retired with status 0
            // and the returned XferRscIdx equals EP1's allocated resource 1.
            "ep1-clean" => {
                TRACE_HARVEST_EP1 != 0xFFFF_FFFF
                    && (TRACE_HARVEST_EP1 & 0x8000_0000) == 0
                    && (TRACE_HARVEST_EP1 & 0x7f_ffff) == 0x1_0000
            }
            "ep1-done" => {
                TRACE_HARVEST_EP1 != 0xFFFF_FFFF
                    && (TRACE_HARVEST_EP1 & 0x8000_0000) == 0
                    && (TRACE_HARVEST_EP1 & 0xf000) == 0
            }
            "ep1-nores" => {
                TRACE_HARVEST_EP1 != 0xFFFF_FFFF
                    && (TRACE_HARVEST_EP1 & 0x8000_0000) == 0
                    && (TRACE_HARVEST_EP1 & 0xf000) == 0x1000
            }
            // The DEPCMD status is bits 15:12. Ignore CMDIOC and any other
            // non-status completion bits: a zero status nibble is a command
            // success even when the raw register is not exactly zero.
            "ep1-status-success" => {
                TRACE_HARVEST_EP1 != 0xFFFF_FFFF && (TRACE_HARVEST_EP1 & 0xf000) == 0
            }
            "ep1-status-nores" => {
                TRACE_HARVEST_EP1 != 0xFFFF_FFFF && (TRACE_HARVEST_EP1 & 0xf000) == 0x1000
            }
            "ep1-status-bus-expiry" => {
                TRACE_HARVEST_EP1 != 0xFFFF_FFFF && (TRACE_HARVEST_EP1 & 0xf000) == 0x2000
            }
            "ep1-status-other" => {
                TRACE_HARVEST_EP1 != 0xFFFF_FFFF
                    && (TRACE_HARVEST_EP1 & 0xf000) != 0
                    && (TRACE_HARVEST_EP1 & 0xf000) != 0x1000
                    && (TRACE_HARVEST_EP1 & 0xf000) != 0x2000
            }
            // Final data-phase arm outcome after the bounded retry ("DARM").
            "darm" => TRACE_HARVEST_DARM == 0x1_0001,
            "darm-fail" => TRACE_HARVEST_DARM == 0x1_0000,
            // Data-phase TRB outcome: did the core COMPLETE the armed data
            // transfer (0x8 = healthy LST|IOC), and did it report the data
            // phase ready (XferNotReady) before any IN token was answered?
            "ep1-xfer" => TRACE_HARVEST_EP1_XFER != 0xFFFF_FFFF,
            "ep1-xfer-ok" => TRACE_HARVEST_EP1_XFER == 0x8,
            "ep1-xfer-err" => {
                TRACE_HARVEST_EP1_XFER != 0xFFFF_FFFF && TRACE_HARVEST_EP1_XFER != 0x8
            }
            "ep1-nrdy" => TRACE_HARVEST_EP1_NRDY > 0,
            // Post-Run/Stop event-DMA differential: bit 0 = GETEPSTATE
            // returned, bit 1 = the live EP0 transfer remained armed, bit 2 =
            // the event ring received a word. The gate is useful only when
            // the optional probe was compiled in; a missing record is false.
            "post" => TRACE_HARVEST_POST == 0x1_0007,
            "post-command" => TRACE_HARVEST_POST != 0xFFFF_FFFF && TRACE_HARVEST_POST & 1 != 0,
            "post-armed" => TRACE_HARVEST_POST != 0xFFFF_FFFF && TRACE_HARVEST_POST & 2 != 0,
            "post-event" => TRACE_HARVEST_POST != 0xFFFF_FFFF && TRACE_HARVEST_POST & 4 != 0,
            "post-record" => TRACE_HARVEST_POST != 0xFFFF_FFFF,
            "wdt-armed" => WDT_KPSS_EN_AT_ENTRY & 1 != 0,
            "wdt-off" => WDT_KPSS_EN_AT_ENTRY != 0xFFFF_FFFF && WDT_KPSS_EN_AT_ENTRY & 1 == 0,
            // Secure-watchdog SMC result readout (set at probe entry, before
            // the observation window): low word 0 = TZ accepted the disable
            // (high word = attempt index 1 = SMC_64, 2 = SMC_32).
            "swdd-ok" => (SWDD_RESULT & 0xFFFF_FFFF) == 0,
            "swdd-fail" => (SWDD_RESULT & 0xFFFF_FFFF) != 0,
            // SCM path diagnostics from the IS_CALL_AVAIL probe (probe
            // entry): did the SMC interface answer at all, and does the TZ
            // implement (SVC_BOOT, SEC_WDOG_DIS)?
            "scm-answ" => (SWDD_AVAIL & 0xFFFF_FFFF) != 0xFFFF_FFFF,
            "scm-avail" => (SWDD_AVAIL & 0xFFFF_FFFF) == 1,
            "scm-noimpl" => (SWDD_AVAIL & 0xFFFF_FFFF) == 0,
            "scm-dead" => (SWDD_AVAIL & 0xFFFF_FFFF) == 0xFFFF_FFFF,
            // EL3 SMCCC liveness (SMCCC_VERSION answer): major<<16|minor
            // with major >= 1, i.e. a value above 0xFFFF.
            "std-ok" => SWDD_STD != 0xFFFF_FFFF && (SWDD_STD & 0xFFFF_FFFF) > 0xFFFF,
            "std-dead" => SWDD_STD == 0xFFFF_FFFF,
            // Exception-level context at probe entry: is SMC from EL1
            // trapped to EL2 (MDCR_EL2.SMC, bit 14), and at which EL are
            // we actually running (0b0101 = EL1h, 0b1000 = EL2h)?
            "mdcr-trap" => MDCR_EL2_AT_ENTRY & (1 << 14) != 0,
            "mdcr-clean" => MDCR_EL2_AT_ENTRY != u64::MAX && MDCR_EL2_AT_ENTRY & (1 << 14) == 0,
            "el1" => CURRENT_EL_AT_ENTRY & 0xF == 0b0100,
            "el2" => CURRENT_EL_AT_ENTRY & 0xF == 0b1000,
            // Live controller-state probes at gate-eval time (readout for the
            // "SETUP TRB never armed / no events processed" diagnosis): is the
            // device link ON (USBLNKST==0), is the core halted, are the
            // endpoints ready, is the SETUP TRB armed, and is EP0 in the
            // Setup state?
            "lnk-on" => (read(DSTS) >> 18) & 0xf == 0,
            // Did the core's link FSM read U0 at ANY poll sample since boot
            // (latched in link_on_sample), even if it dropped again before
            // this gate's evaluation?
            "lnk-ever-on" => LNK_EVER_ON,
            "lnk-reset" => (read(DSTS) >> 18) & 0xf == 1,
            "lnk-suspend" => {
                let lnkst = (read(DSTS) >> 18) & 0xf;
                lnkst >= 5 && lnkst != 0xf
            }
            "halt" => read(DSTS) & DSTS_DEVCTRLHLT != 0,
            "epready" => ENDPOINTS_READY,
            "ep0armed" => EP0_SETUP_ARMED,
            "ep0setup" => EP0_STATE == Ep0State::Setup,
            // Direct post-handoff recovery result. Unlike `ep0armed`, this
            // preserves the failure stage even when the poll loop later
            // clears or retries the software state. The numeric values are
            // U0_ARM_STATUS: 0 = armed/deferred success, 1 = Run/Stop
            // failure, 4 = DEPSTARTCFG failure, 5/6 = EP0 config failure,
            // and 8 = SETUP STARTTRANSFER did not retire after Run/Stop.
            "u0-status0" => U0_ARM_STATUS == 0,
            "u0-status1" => U0_ARM_STATUS == 1,
            "u0-status4" => U0_ARM_STATUS == 4,
            "u0-status5" => U0_ARM_STATUS == 5,
            "u0-status6" => U0_ARM_STATUS == 6,
            "u0-status8" => U0_ARM_STATUS == 8,
            // Direct-path (init_with_super_speed) EP command sequence: how far
            // did the init get (is4=DEPSTARTCFG issued, is5=DEPCFG ep0,
            // is6=DEPCFG ep1), did the DEPSTARTCFG/DEPCFG command retire
            // (CMDACT bit 10 clear == done, set == the core never processed
            // it), and was the core ready (DCNRD bit 29) / halted (bit 22) /
            // link U0 at the first endpoint command?
            // Pre-DEPSTARTCFG progress: is2 = core reset + global control
            // done (a FALSE here names a core_soft_reset/CSFTRST failure),
            // is3 = post-reset setup + SMMU boundary done.
            "is2" => INIT_STAGE >= 2,
            "is3" => INIT_STAGE >= 3,
            "is4" => INIT_STAGE >= 4,
            "is5" => INIT_STAGE >= 5,
            "is6" => INIT_STAGE >= 6,
            // Core device-state at the moment the FIRST endpoint command was
            // ISSUED (DSTS.DEVCTRL field, bits 13:11): 0 = Reset, 1 =
            // Run/Stop, 5 = Suspend. The post-command DSTS captures are
            // post-timeout and cannot distinguish "never processed" from
            // "processed late".
            "ds-pre-rst" => {
                INIT_DEPSTART_PRE_DSTS != 0xFFFF_FFFF && (INIT_DEPSTART_PRE_DSTS >> 11) & 0x7 == 0
            }
            "ds-pre-rs" => {
                INIT_DEPSTART_PRE_DSTS != 0xFFFF_FFFF && (INIT_DEPSTART_PRE_DSTS >> 11) & 0x7 == 1
            }
            "ds-pre-susp" => {
                INIT_DEPSTART_PRE_DSTS != 0xFFFF_FFFF && (INIT_DEPSTART_PRE_DSTS >> 11) & 0x7 == 5
            }
            "ds-pre-halt" => {
                INIT_DEPSTART_PRE_DSTS != 0xFFFF_FFFF
                    && INIT_DEPSTART_PRE_DSTS & DSTS_DEVCTRLHLT != 0
            }
            // Live core state at gate-eval time (after init + fallback +
            // observation window): which device state does the core sit in,
            // and what is software asking for in DCTL?
            "dsts-rs" => (read(DSTS) >> 11) & 0x7 == 1,
            "dsts-rst" => (read(DSTS) >> 11) & 0x7 == 0,
            "dctl-run" => read(DCTL) & DCTL_RUN_STOP != 0,
            "dctl-csf" => read(DCTL) & DCTL_CSFTRST != 0,
            // Register-file liveness at gate eval: an all-ones readback
            // means the DWC3 aperture is unreachable (core clock/power down)
            // - a different failure class from a stuck reset handshake.
            "dctl-ok" => read(DCTL) != 0xFFFF_FFFF,
            "dsts-ok" => read(DSTS) != 0xFFFF_FFFF,
            // Core state at the moment the soft reset started (the
            // inheritance from the Fastboot teardown, before CSFTRST).
            "pre-res-rst" => {
                INIT_PRE_RESET_DSTS != 0xFFFF_FFFF && (INIT_PRE_RESET_DSTS >> 11) & 0x7 == 0
            }
            "pre-res-rs" => {
                INIT_PRE_RESET_DSTS != 0xFFFF_FFFF && (INIT_PRE_RESET_DSTS >> 11) & 0x7 == 1
            }
            "pre-res-susp" => {
                INIT_PRE_RESET_DSTS != 0xFFFF_FFFF && (INIT_PRE_RESET_DSTS >> 11) & 0x7 == 5
            }
            "pre-res-halt" => {
                INIT_PRE_RESET_DSTS != 0xFFFF_FFFF && INIT_PRE_RESET_DSTS & DSTS_DEVCTRLHLT != 0
            }
            // EP0 IN (physical endpoint 1) command outcome, mirroring the
            // DEPSTARTCFG/EP0-OUT gates.
            "ep1-stuck" => INIT_EPCFG1_RAW != 0xFFFF_FFFF && INIT_EPCFG1_RAW & DEPCMD_CMDACT != 0,
            "ep1-lnk0" => INIT_EPCFG1_RAW != 0xFFFF_FFFF && (INIT_EPCFG1_DSTS >> 18) & 0xf == 0,
            // Retained-trace harvest of the DEPSTARTCFG / SETTRANSFRESOURCE
            // commands (bit 12 = the command timed out with CMDACT stuck):
            // the harvest re-reads this run's live trace at gate eval, so
            // these cross-check the INIT_* snapshot gates above.
            "cfg-hv" => TRACE_HARVEST_CFG != 0xFFFF_FFFF,
            "cfg-hv-done" => {
                TRACE_HARVEST_CFG != 0xFFFF_FFFF && TRACE_HARVEST_CFG & 0x8000_0000 == 0
            }
            "cfg-hv-stuck" => TRACE_HARVEST_CFG & 0x8000_0000 != 0,
            "rsc-hv" => TRACE_HARVEST_RSC != 0xFFFF_FFFF,
            "rsc-hv-done" => {
                TRACE_HARVEST_RSC != 0xFFFF_FFFF && TRACE_HARVEST_RSC & 0x8000_0000 == 0
            }
            "rsc-hv-stuck" => TRACE_HARVEST_RSC & 0x8000_0000 != 0,
            "ds-stuck" => {
                INIT_DEPSTART_RAW != 0xFFFF_FFFF && INIT_DEPSTART_RAW & DEPCMD_CMDACT != 0
            }
            "ds-done" => INIT_DEPSTART_RAW != 0xFFFF_FFFF && INIT_DEPSTART_RAW & DEPCMD_CMDACT == 0,
            "ep0-stuck" => INIT_EPCFG0_RAW != 0xFFFF_FFFF && INIT_EPCFG0_RAW & DEPCMD_CMDACT != 0,
            "ep0-ok" => INIT_EPCFG0_OK,
            "ep1-ok" => INIT_EPCFG1_OK,
            "ds-dcnrd" => INIT_DEPSTART_RAW != 0xFFFF_FFFF && INIT_DEPSTART_DSTS & DSTS_DCNRD != 0,
            "ep0-dcnrd" => INIT_EPCFG0_RAW != 0xFFFF_FFFF && INIT_EPCFG0_DSTS & DSTS_DCNRD != 0,
            "ds-halt" => {
                INIT_DEPSTART_RAW != 0xFFFF_FFFF && INIT_DEPSTART_DSTS & DSTS_DEVCTRLHLT != 0
            }
            "ds-lnk0" => INIT_DEPSTART_RAW != 0xFFFF_FFFF && (INIT_DEPSTART_DSTS >> 18) & 0xf == 0,
            "ep0-lnk0" => INIT_EPCFG0_RAW != 0xFFFF_FFFF && (INIT_EPCFG0_DSTS >> 18) & 0xf == 0,
            other => u32::from_str_radix(other.trim_start_matches("0x"), 16)
                .map(|value| TRACE_HARVEST == value)
                .unwrap_or(false),
        };
        Some(ok)
    }
}

/// Park for `seconds` (no pull-up toggling, no fallback path, no secondary
/// attempt, so gate readouts stay uncontaminated), then reset through PSCI
/// SYSTEM_RESET with the Qualcomm PS_HOLD release as the rejected-SMC
/// fallback (inlined here because usb_probe is a separate binary). The trace
/// marker carries the park duration so a later retained-trace read can name
/// the exact branch.
pub fn park_for_seconds(seconds: u64) -> ! {
    unsafe {
        trace_marker(
            TRACE_PROBE_WATCHDOG,
            0x5041_524B | ((seconds & 0xff) as u32) << 8,
        ); // "PARK"+secs
        let frequency = arch_counter_frequency();
        let deadline = arch_counter().saturating_add(frequency.saturating_mul(seconds));
        // Power keepalive: the restored USB domain is still collapsed by
        // RPMh ~5-8 s after the attach wakes it, even with the initial vote
        // set. Re-assert the rail votes and the GDSC enable periodically
        // (never the reset lines: those would kill a live controller) so
        // parks run to their own deadline. Pure spin, no other MMIO.
        let keepalive_period = frequency.saturating_div(2);
        let mut next_keepalive = arch_counter().saturating_add(keepalive_period);
        while frequency != 0 && arch_counter() < deadline {
            wdt_pet();
            if arch_counter() >= next_keepalive {
                // apply_usb_power early-returns once its state flag matches,
                // so the periodic rail re-vote must go through the refresh
                // path: the GDSC force-enable below cannot hold a domain
                // whose CX corner and interconnect vote RPMh has already
                // dropped.
                let _ = super::platform::bramble::refresh_usb_domain_votes(
                    super::platform::bramble::UsbBusVote::Nominal,
                    true,
                );
                let _ = super::platform::bramble::force_enable_usb30_gdsc();
                next_keepalive = arch_counter().saturating_add(keepalive_period);
            }
            core::hint::spin_loop();
        }
        unsafe {
            // PSCI SYSTEM_RESET (function 9) FIRST. The PS_HOLD release
            // lives in the PMIC/SPMI aperture, which the probe path never
            // clocks up; on this board that write can stall the CPU, handing
            // recovery to the secure watchdog (~37 s return) instead of the
            // PSCI reset. If the SMC returns (firmware rejected it), release
            // PS_HOLD behind it and let the watchdog finish recovery.
            // The old #7 encoding was MIGRATE_INFO_UP_CPU and could return
            // without resetting; SYSTEM_RESET is function 9 (0x84000009).
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
            core::ptr::write_volatile(0x0c26_4000usize as *mut u32, 0);
        }
        loop {
            core::hint::spin_loop();
        }
    }
}

/// Park the probe after a gate readout failed. Bounded: after 90 s the probe
/// resets through the normal recovery path even if the assembly timer is
/// late.
pub fn park_after_gate_failure() -> ! {
    park_for_seconds(90)
}

/// Deassert the pull-up so the host sees a physical disconnect.
///
/// The Qualcomm session overrides are cleared INSTEAD of toggling
/// DCTL.Run/Stop: a wedged core ignores DCTL, but the QSCRATCH session votes
/// reach the PHY directly and still control the physical pull-up.
pub fn ep0_signal_drop_pullup() {
    unsafe {
        let ss = read_qscratch(QSCRATCH_SS_PHY_CTRL);
        write_qscratch(QSCRATCH_SS_PHY_CTRL, ss & !(1 << 24));
        let hs = read_qscratch(QSCRATCH_HS_PHY_CTRL);
        write_qscratch(QSCRATCH_HS_PHY_CTRL, hs & !((1 << 20) | (1 << 28)));
        let _ = read_qscratch(QSCRATCH_HS_PHY_CTRL);
        if option_env!("FULLERENE_USB_SIGNAL_DROP_VBUS") == Some("1") {
            // The QUSB2 PHY's VBUSVLDEXT0 forces session-valid at the PHY, so
            // it can own the pull-up independently of DCTL and the QSCRATCH
            // session bits. Clear it (and its select latch) to test that
            // ownership with a host-visible disconnect/re-attach pair.
            hsphy_update(HSPHY_CTRL1, HSPHY_CTRL1_VBUSVLDEXT0, 0);
            hsphy_update(HSPHY_COMMON1, HSPHY_COMMON1_VBUSVLDEXTSEL0, 0);
        }
        // On Bramble the Qualcomm session/VBUS override bits are not the
        // host-visible pull-up owner: clearing them alone still allowed the
        // host to reach HS attach in the code-9 control.  Use the same DCTL
        // Run/Stop path as the proven host-visible gate after removing the
        // glue overrides.  This remains diagnostic-only and deliberately
        // skips the halt readback so a wedged core cannot hide the drop.
        let _ = run_stop_device_no_readback(false);
    }
}

/// Diagnostic clock-source flip for the voteflip gate: repeat the
/// Qualcomm UTMI-as-PIPE selection sequence while the host is driving the
/// descriptor read. If the core's USB2 RX dies during this window, the
/// host journal shows a disconnect; if the -110 window completes normally
/// with -110 at attach+5 s, the clock mux is inert for enumeration.
pub fn flip_utmi_pipe_clock() {
    unsafe {
        select_utmi_pipe_clock();
        let _ = read_qscratch(QSCRATCH_GENERAL_CFG);
    }
}

/// Raw-write variant of the diagnostic clock-source flip for the
/// voteflip2 gate: drive QSCRATCH_GENERAL_CFG directly instead of through
/// the read-modify-write helper, ending at the restored UTMI-as-PIPE
/// value. Distinguishes a qscratch_set() path bug from a PHY-side clock
/// event that kills the core's RX regardless of the write encoding.
pub fn flip_utmi_pipe_clock_raw() {
    unsafe {
        write_qscratch(QSCRATCH_GENERAL_CFG, PIPE_UTMI_CLK_DIS);
        crate::timer::delay_us(100);
        write_qscratch(QSCRATCH_GENERAL_CFG, PIPE_UTMI_CLK_SEL | PIPE3_PHYSTATUS_SW);
        crate::timer::delay_us(100);
        write_qscratch(QSCRATCH_GENERAL_CFG, PIPE_UTMI_CLK_SEL | PIPE3_PHYSTATUS_SW);
    }
}

/// Restore the Qualcomm glue session overrides after a signal-probe vote
/// experiment. Mirrors the handoff's vbus_override sequence exactly; used
/// by the voteflip gate so the attach survives the toggle.
pub fn restore_usb2_session_votes() {
    unsafe {
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        let _ = read_qscratch(QSCRATCH_HS_PHY_CTRL);
    }
}

/// Publish the physical pull-up from the signal probe after a failed
/// handoff. Restores the Qualcomm session overrides and Run/Stop so the
/// diagnostic gates remain host-visible even when init failed before its own
/// Run/Stop boundary (e.g. the pre-connect STARTTRANSFER differential).
/// Stop the core through DCTL Run/Stop (the inverse of the handoff's
/// soft-connect). Unlike ep0_signal_drop_pullup - which clears the QSCRATCH
/// session votes and is host-invisible on this board - the DWC3 Run/Stop bit
/// owns the physical pull-up: stopping the core while the host still tracks
/// the device publishes a real "USB disconnect" line in the host kernel
/// log. This is the one-bit gate-TRUE readout; the old SDIS-named path and
/// the QSCRATCH drop are both dead channels here. The wait acknowledges device
/// events while halting per the databook stop contract.
pub fn gate_true_stop_device() -> bool {
    unsafe { run_stop_device(false) }
}

/// Stop the gadget at a diagnostic boundary without waiting for the DWC3
/// halted-state readback. This keeps a SETUP-boundary probe inside the host's
/// pending control-transfer window; the ordinary gate helper remains the
/// readback-checked path for non-timing-sensitive tests.
pub fn gate_true_stop_device_fast() -> bool {
    unsafe { run_stop_device_no_readback(false) }
}

/// Public Run/Stop re-assert for the dstat readout: the host-visible side of
/// the stop/run cycle (Run/Stop owns the physical pull-up).
pub fn gate_true_run_device() -> bool {
    unsafe { run_stop_device(true) }
}

/// One-shot core-domain snapshot for the attach-delay readout: the gate-run
/// probe delays the physical attach by `code * 80 ms`, and the host kernel
/// log's attach timestamp publishes the code (±0.1 s, far inside the ~17 s
/// biter window that overrides every park and reset channel).
///
/// Encoding (6 bits, 0..63 -> 0..5.04 s):
///   bits 5:3 = raw GDSCR bits 2:0 (SW_COLLAPSE | PWR_ON-ish state words)
///   bit  2   = GSNPSID reads a DWC3 revision (the core answers MMIO)
///   bit  1   = DSTS.DEVCTRLHLT set
///   bit  0   = DSTS USBLNKST nibble nonzero
pub fn core_attach_delay_code() -> u32 {
    unsafe {
        let gdscr = core::ptr::read_volatile(
            &super::platform::bramble::usb_resources().gdsc as *const usize as *const u8
                as *const u32,
        );
        let snpsid = read(GSNPSID);
        let dsts = read(DSTS);
        let gdscr_bits = (gdscr & 0x7) << 3;
        let snpsid_ok = known_dwc_core_ip(snpsid) as u32;
        let halt = (dsts & DSTS_DEVCTRLHLT != 0) as u32;
        let link = ((dsts >> 18) & 0xf != 0) as u32;
        gdscr_bits | (snpsid_ok << 2) | (halt << 1) | link
    }
}

/// Power-recovery stage test for the attach-delay readout (4 bits):
///   bit 0 = the RPMh rail votes ACKed through the normal USB2 power path
///   bit 1 = the forced GDSC enable reached PWR_ON
///   bit 2 = GDSCR reads back with SW_COLLAPSE cleared
///   bit 3 = GSNPSID answers a DWC3 revision (the core answers MMIO)
pub fn core_power_recovery_code() -> u32 {
    unsafe {
        let votes = super::platform::bramble::apply_usb_power(true, false);
        let gdsc_on = super::platform::bramble::force_enable_usb30_gdsc();
        let gdscr = core::ptr::read_volatile(
            &super::platform::bramble::usb_resources().gdsc as *const usize as *const u8
                as *const u32,
        );
        let collapse_cleared = (gdscr & 1 == 0) as u32;
        let snpsid = read(GSNPSID);
        let snpsid_ok = known_dwc_core_ip(snpsid) as u32;
        votes as u32 | ((gdsc_on as u32) << 1) | (collapse_cleared << 2) | (snpsid_ok << 3)
    }
}

/// Full core-recovery sequence (rails + GDSC + clocks + reset pulses) and a
/// binary attach-delay readout: the attach lands ~0.5 s after entry when the
/// DWC3 core still fails to answer GSNPSID, and ~4.0 s after entry when the
/// core answers. The 3.5 s separation is far outside the ±0.5 s attach noise
/// and stays inside the biter window.
pub fn core_alive_attach_delay_secs() -> u64 {
    unsafe {
        let _ = super::platform::bramble::apply_usb_power(true, false);
        let _ = super::platform::bramble::force_enable_usb30_gdsc();
        let _ = super::platform::bramble::usb_clock::configure_usb_clocks(
            super::platform::bramble::UsbBusVote::Nominal,
        );
        let _ = super::platform::bramble::enable_usb_clock_branches();
        let _ = super::platform::bramble::usb_reset::reset_usb_blocks(false);
        let snpsid = read(GSNPSID);
        let snpsid_ok = known_dwc_core_ip(snpsid);
        if snpsid_ok { 4_000 } else { 500 }
    }
}

pub fn ep0_signal_publish_pullup() {
    unsafe {
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(QSCRATCH_HS_PHY_CTRL, (1 << 20) | (1 << 28));
        let _ = run_stop_device(true);
    }
}

/// Reassert the pull-up after a signal drop by restoring the same Qualcomm
/// session overrides the handoff applies.
pub fn ep0_signal_restore_pullup() {
    unsafe {
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(QSCRATCH_HS_PHY_CTRL, (1 << 20) | (1 << 28));
        if option_env!("FULLERENE_USB_SIGNAL_DROP_VBUS") == Some("1") {
            hsphy_update(
                HSPHY_COMMON1,
                HSPHY_COMMON1_VBUSVLDEXTSEL0,
                HSPHY_COMMON1_VBUSVLDEXTSEL0,
            );
            hsphy_update(
                HSPHY_CTRL1,
                HSPHY_CTRL1_VBUSVLDEXT0,
                HSPHY_CTRL1_VBUSVLDEXT0,
            );
        }
    }
}

/// Post-init-failure self-heal, run from the signal probe's polling context
/// after a failed handoff. The host is already attached to the session
/// pull-up (see the -110 runs), so whatever init stage gave up, the missing
/// tail can still be issued here. The order mirrors Linux's
/// `dwc3_gadget_soft_connect`: event buffers, then DEPSTARTCFG /
/// SETEPCONFIG / the EP0 OUT STARTTRANSFER while the core is still in its
/// post-reset state, and ONLY THEN Run/Stop. The DCFG device address is
/// cleared because the bootloader's fastboot address must not survive into
/// the new enumeration (a stale non-zero DEVADDR makes the core ignore the
/// host's default-address SETUP tokens). Gated by a build-time env var; the
/// the status code is kept for the retained trace and the Run/Stop-pair
/// readout.
pub fn u0_arm_recovery() -> u32 {
    if option_env!("FULLERENE_USB_U0_ARM_PROBE")
        .filter(|value| *value != "0")
        .is_none()
    {
        return 0xFFFF_FFFF;
    }
    unsafe {
        if EP0_SETUP_ARMED && ENDPOINTS_READY {
            U0_ARM_STATUS = 0;
            return 0;
        }
        // Android's gadget restart rebuilds endpoint resources while the
        // device controller is halted, then starts it again. The direct
        // handoff may already have asserted Run/Stop before this recovery
        // path is entered, so make that ordering an explicit Bramble A/B.
        // Without the stop, DEPSTARTCFG/SETEPCONFIG can retire while the
        // controller is still running but leave STARTTRANSFER with status 1
        // (No Resource) on this unit.
        if option_env!("FULLERENE_USB_U0_ARM_STOP_FIRST") == Some("1")
            && read(DCTL) & DCTL_RUN_STOP != 0
            && !run_stop_device(false)
        {
            U0_ARM_STATUS = 1;
            return 1;
        }
        let event_address = ep0_event_address();
        cache_clean(ep0_event_dma_base(), ep0_event_size());
        write(GEVNTADRLO0, event_address as u32);
        write(GEVNTADRHI0, (event_address >> 32) as u32);
        write(GEVNTSIZ0, ep0_event_size() as u32);
        acknowledge_ep0_event_count();
        EVENT_OFFSET = 0;
        // The recovery path runs after the Fastboot handoff boundary. DSTS
        // still reports the previous SS Fastboot session there, so trusting
        // ConnectSpd can restore DCFG.SuperSpeed on a USB2-only handoff. The
        // "forcehs" experiment proves or refutes exactly that stale-speed
        // failure mode by pinning recovery to High-Speed/64-byte EP0.
        // "gdbforce" repeats that experiment while sampling GDBGLTSSM at the
        // normal gate window, separating a stale DCFG speed setting from the
        // observed GDBGLTSSM link state.
        let stale_speed = read(DSTS) & DSTS_CONNECTSPD_MASK;
        let force_hs = option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("forcehs")
            || option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("gdbforce");
        let speed = if force_hs { 0 } else { stale_speed };
        let mut dcfg = read(DCFG) & !(DCFG_SPEED_MASK | DCFG_DEVADDR_MASK);
        dcfg |= if force_hs {
            DCFG_HIGHSPEED
        } else if speed == DSTS_SUPERSPEED {
            DCFG_SUPERSPEED
        } else {
            DCFG_HIGHSPEED
        };
        write(DCFG, dcfg);
        GadgetDriver::reset(gadget_mut());
        udc_mut().reset();
        EP0_STATE = Ep0State::Setup;
        CONFIGURED = false;
        DATA_ENDPOINTS_READY = false;
        DATA_REQUEST_SLOTS = [usize::MAX; 2];
        DATA_RESOURCE_INDEX = [0; 2];
        if !send_ep_command(0, DEPCMD_DEPSTARTCFG, 0, 0, 0) {
            U0_ARM_STATUS = 4;
            return 4;
        }
        let max_packet = if !force_hs && speed == DSTS_SUPERSPEED {
            INITIAL_EP0_MAX_PACKET_SIZE
        } else {
            64
        };
        if !configure_endpoint(0, max_packet, false) {
            U0_ARM_STATUS = 5;
            return 5;
        }
        if !configure_endpoint(1, max_packet, false) {
            U0_ARM_STATUS = 6;
            return 6;
        }
        ENDPOINTS_READY = true;
        write(DALEPENA, 0b11);
        write(DEVTEN, direct_gadget_devten());
        // Prepare the EP0 OUT SETUP TRB now. The STARTTRANSFER decision is
        // bramble-specific: see the start-after-connect note below.
        prepare_ep0_setup_trb();
        // Bramble's normal start-after-connect path deliberately does NOT
        // issue STARTTRANSFER before Run/Stop: a pre-link-ON command can
        // wedge this core's endpoint command engine even when Run/Stop still
        // publishes the pull-up. Keep the recovery path consistent with the
        // proven init path: defer the arm until after Run/Stop, then retry
        // for 100 ms while the link trains to ON.
        let defer_start = cfg!(fullerene_aarch64_usb_gadget_handoff_start_after_connect);
        if defer_start {
            U0_ARM_STATUS = 0;
        } else if start_transfer(0, ep0_trb_ptr(0)) {
            EP0_SETUP_ARMED = true;
            PENDING_SETUP_ARM = false;
        } else {
            U0_ARM_STATUS = 8;
        }
        if !run_stop_device(true) {
            U0_ARM_STATUS = 1;
            return 1;
        }
        if defer_start {
            let deadline =
                arch_counter().saturating_add(arch_counter_frequency().saturating_mul(100) / 1000);
            let mut armed = false;
            while arch_counter() < deadline {
                if EP0_SETUP_ARMED {
                    armed = true;
                    break;
                }
                if try_arm_setup() {
                    armed = true;
                    break;
                }
                super::timer::delay_us(200);
            }
            if !armed {
                U0_ARM_STATUS = 8;
            }
        }
        // U0_ARM_STATUS is 0 (the SETUP TRB is armed or deferred in the
        // proven start-after-connect order) or 8 (STARTTRANSFER did not
        // retire even after the link reached ON).
        U0_ARM_STATUS
    }
}

/// Mid-window rescue for the read/64 -110: the host's descriptor URB keeps
/// retrying its SETUP token until the 5 s `initial_descriptor_timeout`, so
/// a full endpoint re-arm while the host is still polling can complete the
/// stuck enumeration. Device soft reset first: the post-init
/// u0_arm_recovery runs at probe entry with the link down, where Start
/// Transfer is rejected, and any stuck core control state left by the
/// original arm attempt is only cleared by CSFTRST. The host port stays
/// connected while the core is stopped (calibrated: the gate-TRUE
/// Run/Stop stop is host-invisible), so the reset plus the re-arm are
/// invisible and the host simply sees its retries answered. The readout is
/// the enumeration outcome in the host journal (1234:0001 = the re-arm
/// landed; -110 again = it did not). Returns the u0_arm_recovery status.
/// Re-initialize the USB2 PHY after Run/Stop.  This tests whether the PHY
/// RX path can be recovered by re-running the full init sequence while the
/// DWC3 is in Run mode (link ON).  The hypothesis: the Stop→Start cycle
/// loses PHY RX state that can only be restored by re-programming the PHY
/// after the link is established.
pub fn phy_retry_after_link() -> bool {
    unsafe {
        // Wait for link ON (USBLNKST == 0 in DSTS)
        let deadline = super::timer::counter() + 5_000_000_000; // 5s in timer ticks
        while super::timer::counter() < deadline {
            let dsts = read(DSTS);
            if (dsts >> 18) & 0xf == 0 && dsts & DSTS_DEVCTRLHLT == 0 {
                break;
            }
        }
        // Check if we're in U0
        let dsts = read(DSTS);
        if (dsts >> 18) & 0xf != 0 {
            return false;
        }
        // Re-run the full PHY init sequence
        if !cfg!(fullerene_aarch64_usb_skip_usb2_phy_reset) {
            if !super::platform::bramble::pulse_usb2_phy_reset() {
                return false;
            }
        }
        phy::init_hsphy();
        config::configure_usb2_phy_interface();
        // Clear SUSPHY and ENBLSLPM after PHY re-init
        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        mark_g2w_site(1018);
        write(GUSB2PHYCFG0, usb2);
        true
    }
}

pub fn u0_arm_window_recovery() -> u32 {
    unsafe {
        if !device_soft_reset() {
            return 9;
        }
        // Force the full tail: the armed flags may be stale (set by a
        // rejected arm) or accurate; the re-issue is idempotent on a
        // freshly soft-reset core.
        EP0_SETUP_ARMED = false;
        ENDPOINTS_READY = false;
        u0_arm_recovery()
    }
}

/// Queue a host-visible blip readout to be emitted once the link reaches
/// ON. The host only attaches ~10 s after boot, so a blip issued right
/// after the failed handoff would be invisible; the poll loop emits it via
/// `try_u0_blip` when the core reports the link ON.
pub fn u0_arm_set_blips(count: u32) {
    // Clearing is unconditional so a diagnostic gate can take ownership of
    // the transport even when the legacy U0-arm feature was enabled.
    if count != 0
        && option_env!("FULLERENE_USB_U0_ARM_PROBE")
            .filter(|value| *value != "0")
            .is_none()
    {
        return;
    }
    unsafe {
        U0_BLIP_PENDING = count.min(6);
    }
}

/// Queue the host-visible pre-EP0 arm result for the direct handoff. One
/// Run/Stop pair means that SETUP STARTTRANSFER succeeded; two means the
/// DSTS HALT gate rejected the attempt; three means the command completed
/// with an error; four means it timed out; five is an unclassified/no-attempt
/// result. The count is intentionally encoded only in the diagnostic blip;
/// the EP0/TRB path itself is unchanged. If the link is not U0 yet, the
/// pending count remains for the timer poll, which is the reliable late-U0
/// readout. A 30-second fallback makes the marker host-visible even when the
/// device-side link FSM never reaches U0 before host enumeration.
pub fn arm_blip_queue() {
    if option_env!("FULLERENE_USB_ARM_BLIP")
        .filter(|value| *value != "0")
        .is_none()
    {
        return;
    }
    unsafe {
        let count = if EP0_SETUP_ARMED {
            1
        } else {
            match SETUP_ARM_FAILURE_STAGE {
                1 => 2,
                2 => 3,
                3 => 4,
                _ => 5,
            }
        };
        if EP0_SETUP_ARMED {
            U0_BLIP_PENDING = U0_BLIP_PENDING.max(count);
            // Use the same guarded helper as the timer path. It does not
            // force a Run/Stop transition while the link is training, and
            // leaves the queued differential intact for a later U0 sample.
            try_u0_blip();
        } else {
            U0_BLIP_PENDING = U0_BLIP_PENDING.max(count);
        }
        ARM_BLIP_QUEUED = true;
        ARM_BLIP_FORCE_DEADLINE =
            arch_counter().saturating_add(arch_counter_frequency().saturating_mul(30));
        try_u0_blip();
    }
}

/// Emit the queued blips once, when the link is ON. Same link test as
/// `try_arm_setup` (it is also issued after the arm attempt, so the SETUP
/// TRB is in place before the blip's re-attach re-runs enumeration).
unsafe fn try_u0_blip() {
    unsafe {
        if U0_BLIP_PENDING == 0 {
            return;
        }
        let dsts = read(DSTS);
        let force = ARM_BLIP_FORCE_DEADLINE != 0 && arch_counter() >= ARM_BLIP_FORCE_DEADLINE;
        if !force && (dsts & DSTS_DEVCTRLHLT != 0 || (dsts >> 18) & 0xf != 0) {
            return;
        }
        let count = U0_BLIP_PENDING;
        U0_BLIP_PENDING = 0;
        ARM_BLIP_FORCE_DEADLINE = 0;
        ARM_BLIP_DONE = true;
        runstop_blips(count);
    }
}

/// Emit up to `count` Run/Stop blips only when the core reports the link ON
/// (running, unhalted, USBLNKST == 0). A non-U0 link is not a valid transport
/// precondition, so this is a silent no-op.
pub fn runstop_blips_link_on(count: u32) {
    unsafe {
        let dsts = read(DSTS);
        if dsts & DSTS_DEVCTRLHLT != 0 || (dsts >> 18) & 0xf != 0 {
            return;
        }
        runstop_blips(count);
    }
}

/// Host-visible diagnostic transport: toggle the qpr1 DWC3 Run/Stop control
/// `count` times. Each stop/run pair is one disconnect/re-attach pair in the
/// host kernel log. This is deliberately called only after the sampled state
/// has been latched. It is not a read-only observation.
pub fn runstop_blips(count: u32) {
    unsafe {
        for _ in 0..count.min(6) {
            let _ = run_stop_device_no_readback(false);
            super::timer::delay_ms(300);
            let _ = run_stop_device_no_readback(true);
            super::timer::delay_ms(200);
        }
    }
}

/// Fast Run/Stop transport for categorical readouts: 100 ms disconnected +
/// 70 ms reconnected per pair. Six pairs fit in about 1.2 s. The status
/// readback is intentionally skipped here to stay inside the secure-WDT
/// window; the normal handoff still uses the checked source-aligned helper.
pub fn runstop_blips_fast(count: u32) {
    unsafe {
        let dsts = read(DSTS);
        if dsts & DSTS_DEVCTRLHLT != 0 || (dsts >> 18) & 0xf != 0 {
            return;
        }
        for _ in 0..count.min(6) {
            let _ = run_stop_device_no_readback(false);
            super::timer::delay_ms(100);
            let _ = run_stop_device_no_readback(true);
            super::timer::delay_ms(70);
        }
    }
}

/// Linux enables the DWC3 controller SPI immediately after arming the first
/// EP0 OUT SETUP TRB. The standalone probe's assembly entry prepares the
/// exception vector and CPU interface, but the Distributor still needs the
/// normal Rust GIC initialization before a USB SPI can be delivered. Keep
/// this probe-only: the normal Fullerene boot path owns GIC setup after USB
/// initialization and must not receive an early IRQ.
#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
unsafe fn enable_gadget_controller_irq() {
    unsafe {
        let _ = super::platform::gicv3::init(
            super::platform::bramble::GICD_BASE,
            super::platform::bramble::GICR_BASE,
            Some(super::platform::bramble::USB_DWC3_IRQ),
        );
    }
}

#[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
unsafe fn enable_gadget_controller_irq() {}

/// Poll the DWC3 event ring. This is intentionally cheap enough to run from
/// the early boot loop until the normal interrupt controller owns the device.
pub fn poll() {
    unsafe {
        // Proof that the driver loop ran, written where every crate can read it. There is
        // no other crate-independent way to say "poll() executed": a `static mut` is
        // duplicated by the second `usb/` compilation.
        //
        // NOT latched, deliberately. The first version guarded this with a `static mut
        // bool` and it read false on a second run: `.bss` is not guaranteed to be cleared
        // between `fastboot boot` attempts the way the retained region is not either, and
        // a latch that starts true can never write its marker. Writing every pass instead
        // is safe here because the ring is 256 entries and `prev_boot_poll_ran()` asks
        // "is there a POL1 anywhere in the ring" - a marker repeated every pass survives
        // even a full wrap, and a marker that is never written stays absent.
        // See usb/README.md §3.12.
        trace_marker(TRACE_PROBE_WATCHDOG, 0x504F_4C31); // "POL1"
        // Diagnostic quiet window (see mmio_quiet_active): after this many
        // seconds past the first Run/Stop, stop ALL controller MMIO access.
        if mmio_quiet_active() {
            return;
        }
        // Eventless SETUP path: `GEVNTCOUNT0` stays 0 on this handoff even
        // though the host's traffic reaches the controller, so the EP0 SETUP
        // buffer is polled directly to answer the host's GET_DESCRIPTOR.
        // A no-op unless `--utmi-postrun-readout usb2-live-setup-poll`.
        let _ = poll_setup_buffer();
        link_on_sample();
        let runtime = USB_RUNTIME_STATE;
        // In the no-SMMU differential the whole point is to never touch the
        // Apps-SMMU: the stream is unmatched there and the (inactive, often
        // clock-gated) SMMU aperture can fault the CPU with an asynchronous
        // external abort when its runtime clock gates later in the session,
        // which reboots the handset right in the middle of host enumeration.
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_no_smmu)
            && !matches!(
                runtime,
                super::platform::bramble::UsbRuntimeState::Off
                    | super::platform::bramble::UsbRuntimeState::Suspended
            )
        {
            service_smmu_fault();
        }
        service_power_event();
        // Complete a PMIC Type-C parent IRQ in normal polling context, after
        // the hard IRQ path has masked the parent. This is the early-boot
        // equivalent of Linux's threaded qcom-pmic-typec handler and keeps
        // slow SPMI transactions out of the DWC3 interrupt entry.
        service_deferred_platform();
        service_usb2_runtime_power_keepalive();
        if RESUME_PENDING {
            RESUME_PENDING = false;
            if CONFIGURED && !runtime_resume() {
                // Keep the request pending if clocks/PHY are not yet ready;
                // the next poll then retries just as Linux's resume work does.
                RESUME_PENDING = true;
            }
        }
        // Signal builds must keep exactly one actuator (the diagnostic
        // pull-up toggle): a Type-C poll that samples a transient CC state
        // would otherwise apply an uncontrolled detach and pollute the
        // attach/disconnect readouts.
        if !cfg!(fullerene_aarch64_usb_ep0_signal_probe) {
            poll_typec_state(false);
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_arm_window_recovery)]
        if RUN_STOP_TICK != 0
            && !CONFIGURED
            && EP0_STATE == Ep0State::Setup
            && !EP0_SETUP_ARMED
            && ARM_WINDOW_RECOVERY_ATTEMPTS < ARM_WINDOW_RECOVERY_MAX_ATTEMPTS
            && arch_counter_frequency() != 0
        {
            // The direct path can publish the pull-up long before xHCI
            // reaches the USB2 port. The first recovery is intentionally
            // late, and failed recoveries are retried at a bounded cadence:
            // on this board HS attach is ~39 s after Run/Stop while the
            // descriptor request arrives ~5 s later. Do not reset a live,
            // already-armed gadget; the attempt counter advances only when
            // the scheduled recovery is actually due.
            let elapsed = arch_counter().saturating_sub(RUN_STOP_TICK);
            let retry_after = ARM_WINDOW_RECOVERY_DELAY_SECS
                + u64::from(ARM_WINDOW_RECOVERY_ATTEMPTS)
                    .saturating_mul(ARM_WINDOW_RECOVERY_RETRY_SECS);
            if elapsed >= arch_counter_frequency().saturating_mul(retry_after) {
                ARM_WINDOW_RECOVERY_ATTEMPTS = ARM_WINDOW_RECOVERY_ATTEMPTS.saturating_add(1);
                log_puts("usb gadget handoff: late automatic EP0 recovery\n");
                let status = u0_arm_window_recovery();
                U0_ARM_STATUS = status;
                trace_event(
                    TRACE_SETUP_QUEUED,
                    0x5253_434C, // "RSCL": late arm-window recovery result
                    status,
                    EP0_SETUP_ARMED as u32,
                    u32::from(ARM_WINDOW_RECOVERY_ATTEMPTS),
                    read(DSTS),
                );
            }
        }
        let mut event_seen = poll_ep0_event_ring();
        // If the event FIFO is empty, give the opt-in DMA fallback one chance
        // to dispatch a controller-retired EP0 TRB.  The fallback is skipped
        // whenever a real event batch was consumed, so it cannot double-run a
        // normal completion.
        if !event_seen {
            event_seen = poll_ep0_trb_completion_fallback();
        }
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        update_signal_latches();
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if option_env!("FULLERENE_USB_SIGNAL_DMA_POST_RUNSTOP") == Some("1")
            && POST_RUNSTOP_PROBE_PENDING
            && arch_counter() >= POST_RUNSTOP_PROBE_NOT_BEFORE
            && read(DCTL) & DCTL_RUN_STOP != 0
        {
            // Run/Stop returns before the host's attach debounce. Perform the
            // probe only after the calibrated delay, after the normal event
            // consumer has handled any reset/connect event, so its
            // STARTTRANSFER/ENDTRANSFER test uses the live EP0 state. Do not
            // gate the diagnostic on DSTS.DEVCTRLHLT: a stale halt bit is one
            // of the hypotheses under test, and the probe records the
            // command/armed/event bits even when the core refuses the
            // read-only command.
            POST_RUNSTOP_PROBE_PENDING = false;
            let _ = post_runstop_event_dma_probe();
        }
        // Attach-time one-bit readout (see `DEFERRED_READOUT_KIND`). This runs
        // here, in the polling owner, rather than inside the handoff, so the
        // reading reflects the state the host actually attached to and reset -
        // which is the only point at which questions about the event machinery
        // can be answered at all.
        if DEFERRED_READOUT_KIND != 0 {
            // Trigger on the host actually driving the bus, not on a fixed delay.
            // Measured timing (`354670.0`, host kernel log): the host detects the
            // attach at ~6.2 s (its debounce), logs "new high-speed USB device
            // number N", and fails the descriptor read only 588 ms later - so the
            // whole host-visible window is ~0.6 s, and the old fixed 8 s delay
            // (`POST_RUNSTOP_PROBE_DELAY_SECS`) fired *after* the host had already
            // given up. `DSTS.USBLNKST` cannot be the trigger because it reads
            // "On" from the very start (`305671.0`), but `DSTS.SOFFN` only
            // advances once the host is sending SOFs, i.e. once it is really
            // talking to us - the first SOF arrives within milliseconds of the
            // reset that precedes the SETUP.
            //
            // The first poll after the handoff only *arms* the trigger with the
            // current frame number; the action fires on the next change. Arming
            // inside the branches would be easy to forget, and run `358131.0`
            // showed what happens without it: with a sentinel initial value the
            // condition was true immediately, so the action ran ~1 s after the
            // handoff, before the host had attached.
            const SOFFN_MASK_POLL: u32 = 0x3fff << 3;
            let soffn = unsafe { read(DSTS) } & SOFFN_MASK_POLL;
            let kind_now = unsafe { DEFERRED_READOUT_KIND };
            // Retained-trace marker: the only instrument left that can prove the
            // deferred block ran at all. The host-visible channels are exhausted -
            // CCS pulses use primitives that are inert on this revision
            // (`usb_probe.rs:1331`) and the Run/Stop gates emit nothing while the
            // device is stuck pre-configuration. This lands in `USB_TRACE` in DRAM,
            // which a later boot can read back through `prev_boot_*`. One-shot: this
            // block is evaluated on every poll, so an unguarded marker would flood
            // the ring and erase the events that explain the handoff.
            if !unsafe { DEFR_MARKED } {
                unsafe {
                    DEFR_MARKED = true;
                    trace_event(
                        TRACE_PROBE_WATCHDOG,
                        0x4445_4652, // "DEFR": the deferred block is being evaluated
                        kind_now,
                        u32::from(arch_counter() >= POST_RUNSTOP_PROBE_NOT_BEFORE),
                        0,
                        read(DSTS),
                    );
                }
            }
            // Kind 5 (deliberate re-attach) wants to run *after* the host has
            // given up, so it uses the time delay; the readout kinds wait for the
            // host to actually drive the bus.
            let fire = if kind_now >= 5 {
                unsafe { arch_counter() >= POST_RUNSTOP_PROBE_NOT_BEFORE }
            } else if !DEFERRED_SOFFN_ARMED {
                DEFERRED_SOFFN_ARMED = true;
                LAST_DEFERRED_SOFFN = soffn;
                false
            } else {
                soffn != LAST_DEFERRED_SOFFN
            };
            if fire {
                LAST_DEFERRED_SOFFN = soffn;
                let kind = DEFERRED_READOUT_KIND;
                DEFERRED_READOUT_KIND = 0;
                if kind == 4 {
                    // Fix candidate: run the event-path bus-reset recovery while
                    // the host is still retrying, instead of never (nothing else
                    // can reach it when the core posts no events - `usb.rs:168`).
                    unsafe { restart_control_after_reset() };
                }
                if kind == 5 {
                    // Deliberate re-attach: the host's first attempt has already
                    // failed (it gives up ~600 ms after attaching - entry 171), so
                    // drop the pull-up, fully rebuild EP0, then re-publish. The
                    // observable is the host kernel log: a second attach followed
                    // by a successful descriptor read needs no CCS channel.
                    unsafe {
                        ep0_signal_drop_pullup();
                        readout_keepalive_delay_ms(400);
                        let _ = u0_arm_window_recovery();
                        readout_keepalive_delay_ms(200);
                        ep0_signal_publish_pullup();
                    }
                }
                if kind == 9 {
                    // Positive control: two unconditional cycles.
                    unsafe { gate_cycle_publish(2) };
                }
                if kind == 7 {
                    // Publish `diag_readout_code()` as extra attach lines.
                    unsafe { gate_cycle_publish(diag_readout_code().clamp(1, 6)) };
                }
                if kind == 8 {
                    // One extra attach line iff EP0's SETUP transfer is armed.
                    if unsafe { EP0_SETUP_ARMED } {
                        unsafe { gate_cycle_publish(1) };
                    }
                }
                if kind == 10 {
                    // One extra attach line iff the endpoint epoch was published.
                    // `gate_cycle_publish`, not `ccs_pulse`: this is the point where
                    // the plain `ccs_pulse` stops reaching the host.
                    if unsafe { ENDPOINTS_READY } {
                        unsafe { gate_cycle_publish(1) };
                    }
                }
                // The CCS pulse channel is skipped for the gate kinds: the pull-up
                // drop primitives are inert on this revision, so a pulse would be
                // invisible and only waste the window before the handset collapses.
                if kind <= 4 {
                    // Must use the no-readback gate: the plain `ccs_pulse` waits on
                    // the Run/Stop halt readback, which is exactly what stops pulses
                    // reaching the host once enumeration has started (`336520.0`,
                    // `350817.0`).
                    unsafe { ccs_pulse_no_readback(1_000) };
                    let bit = unsafe {
                        match kind {
                            1 => {
                                // Is the event ring memory non-zero? The ring retains
                                // what the core wrote, so this distinguishes "the core
                                // never posted" from "posted and consumed".
                                let base = ep0_event_dma_base();
                                cache_invalidate(base, 64);
                                let words = core::slice::from_raw_parts(base as *const u32, 16);
                                words.iter().any(|word| *word != 0)
                            }
                            2 => read(DEVTEN) != 0,
                            3 => read(DCFG) & DCFG_DEVADDR_MASK == 0,
                            _ => false,
                        }
                    };
                    if bit {
                        unsafe { ccs_pulse_no_readback(300) };
                    }
                }
            }
        }
        if !event_seen {
            drain_gsi_event_buffers();
            // The core rejects Start Transfer while the link is not ON (this
            // includes the window right after Run/Stop and the host's bus
            // reset), so the initial SETUP arm can fail. Once the link comes
            // up, arm here: the core then immediately delivers any SETUP
            // packet it latched while no TRB was armed.
            let _ = try_arm_setup();
            try_u0_blip();
            return;
        }
        drain_gsi_event_buffers();
        let _ = try_arm_setup();
        try_u0_blip();
    }
}

/// Service only the DWC3 event ring from the periodic timer interrupt.
///
/// Android-init enters user space through `launchd::run()` and does not return
/// to the boot loop that normally calls `poll()`.  The controller SPI should
/// deliver the same work through `aarch64_exception_irq`, but keeping this
/// narrow timer fallback makes EP0 progress independent of a platform IRQ
/// delivery quirk.  Do not run Type-C, power, or SMMU work here.  The one
/// optional blip is a queued Run/Stop differential whose U0 guard is kept
/// here so Android-init cannot miss it after the short initial arm window.
pub fn poll_from_timer_irq() {
    unsafe {
        if !EARLY_HANDOFF_ACTIVE || mmio_quiet_active() {
            return;
        }
        link_on_sample();
        service_usb2_runtime_power_keepalive();
        let event_seen = poll_ep0_event_ring();
        if !event_seen {
            let _ = poll_ep0_trb_completion_fallback();
        }
        let _ = try_arm_setup();
        // Android-init may reach U0 after the short post-Run/Stop arm
        // window. Keep the queued source-backed Run/Stop differential alive
        // through that later timer poll instead of silently dropping it when
        // the first 100 ms window has elapsed.
        try_u0_blip();
    }
}

/// Consume Qualcomm GSI event buffers. Android reserves event buffers 1..3 for
/// the data path; decode each record as an event word and retain it in the
/// same trace used by EP0. Unknown GSI event encodings are still acknowledged
/// without being mistaken for control transfers.
unsafe fn drain_gsi_event_buffers() {
    let configured = super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count
        .min(3) as usize;
    for index in 0..configured {
        let count_reg = GEVNTCOUNT0 + (index + 1) * GEVNT_BUFFER_STRIDE;
        let count = unsafe { read(count_reg) & 0xfffc } as usize;
        if count == 0 {
            continue;
        }
        let mut remaining = count;
        while remaining >= 4 {
            let offset = unsafe { GSI_EVENT_OFFSETS[index] };
            unsafe {
                cache_invalidate(
                    addr_of!(GSI_EVENTS) as usize + index * EVENT_BUFFER_SIZE + offset,
                    4,
                );
                let event_ptr =
                    (addr_of!(GSI_EVENTS) as *const u8).add(index * EVENT_BUFFER_SIZE + offset);
                let raw = u32::from_le_bytes([
                    read_volatile(event_ptr),
                    read_volatile(event_ptr.add(1)),
                    read_volatile(event_ptr.add(2)),
                    read_volatile(event_ptr.add(3)),
                ]);
                let endpoint = GSI_CHANNEL_ENDPOINT[index] as u8;
                let address = endpoint | if endpoint & 1 != 0 { 0x80 } else { 0 };
                let request_slot = GSI_REQUEST_SLOTS[index];
                let completion_status = (raw >> 12) & 0xf;
                let mut actual = 0;
                if request_slot != usize::MAX {
                    let in_direction = endpoint & 1 != 0;
                    let shape = gsi_ring_shape(in_direction, GSI_DEFAULT_NUM_BUFFERS);
                    let data_index = shape.map(|shape| shape.first_buffer_trb).unwrap_or(0);
                    let ring_base = GSI_RING_BASES[index];
                    let trb = ring_base as usize as *mut Trb;
                    cache_invalidate(
                        ring_base as usize,
                        GSI_RING_TRB_COUNTS[index] * core::mem::size_of::<Trb>(),
                    );
                    if let Some(request) = udc_mut().request(address, request_slot) {
                        let residual =
                            read_volatile(addr_of!((*trb.add(data_index)).size)) & 0x00ff_ffff;
                        actual = request.length.saturating_sub(residual);
                        let _ = udc_mut().complete(
                            address,
                            request_slot,
                            actual,
                            completion_status != 0,
                        );
                        GadgetDriver::on_gsi_data_complete(
                            gadget_mut(),
                            address,
                            actual,
                            completion_status != 0,
                        );
                        let _ = udc_mut().release(address, request_slot);
                    }
                }
                trace_event(
                    TRACE_TRANSFER_COMPLETE,
                    endpoint as u32,
                    raw,
                    offset as u32,
                    actual,
                    count as u32,
                );
                // The event buffer is the ownership boundary for this
                // single-slot early request queue. Keep the event word in
                // retained trace, then make the TRB reusable for the next
                // request.
                GSI_PENDING[index] = false;
                GSI_REQUEST_SLOTS[index] = usize::MAX;
                GSI_RING_ACTIVE[index] = false;
            }
            unsafe {
                GSI_EVENT_OFFSETS[index] = (offset + 4) % EVENT_BUFFER_SIZE;
            }
            remaining -= 4;
        }
        unsafe { write(count_reg, count as u32) };
    }
}
