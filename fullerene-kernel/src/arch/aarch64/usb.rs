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
    configure_gadget_start_defaults,
    configure_usb2_phy_interface, enable_gadget_susphy,
    enable_usb2_gadget_susphy, qscratch_set, run_stop_value,
};
use control::{
    core_soft_reset, device_soft_reset, run_stop_device, run_stop_device_no_readback,
    release_usb3_phy_reset, stop_running_device, write_dctl_safe,
};
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use log::{log_hex, log_hex_value, log_puts};
use mmio::*;
use phy::{
    init_hsphy, init_qmp_phy, qmp_set_autonomous_mode, select_utmi_pipe_clock,
    update_dwc3_ref_clock,
};
pub use phy::{qmp_phase_probe_reached, qmp_phase_probe_requested};
mod config;
mod control;
mod phy;
mod phy_tables;
pub use phy_tables::install_dt_phy_sequences;
use trace::{fill_trace_control_response, trace_begin, trace_event};
pub mod trace;
use trace::*;
pub use trace::{
    TRACE_BOOT_USB_ENTRY, TRACE_EXCEPTION_SYNC, TRACE_PLATFORM_IRQ, TRACE_PROBE_WATCHDOG,
    TRACE_TYPEC_BEGIN, TRACE_TYPEC_DONE, TRACE_TYPEC_EVENT, TRACE_UDC_REARM,
    TRACE_USB_HANDOFF_BEGIN, dump_trace, prev_boot_progress_code, prev_boot_qmp_phase_code,
    trace_head, trace_last_event,
    trace_marker, trace_probe_begin, trace_reset_head_for_boot,
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

unsafe extern "C" {
    static __usb_dma_start: u8;
    static __usb_dma_end: u8;
    static __usb_trace_start: u8;
    static __usb_trace_end: u8;
}

const EVENT_BUFFER_SIZE: usize = 4096;
const MAX_PACKET_SIZE: u32 = 512;
// Linux starts the gadget with the SuperSpeed EP0 descriptor size while the
// link speed is still unknown, then changes it to 64 on a High-Speed
// Connect Done event. The first SETUP transfer must use that initial state.
const INITIAL_EP0_MAX_PACKET_SIZE: u32 = 512;

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
struct DataBuffer([u8; MAX_PACKET_SIZE as usize]);

#[unsafe(link_section = ".usb_dma")]
static mut DATA_OUT_BUFFER: DataBuffer = DataBuffer([0; MAX_PACKET_SIZE as usize]);
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
/// Defer the post-Run/Stop event-DMA probe until the live host link reaches
/// U0. Run/Stop returns before Bramble's host-side attach debounce completes,
/// so an init-time U0 check can finish too early and test a disconnected
/// controller rather than the event path used by enumeration.
static mut POST_RUNSTOP_PROBE_PENDING: bool = false;
static mut POST_RUNSTOP_PROBE_NOT_BEFORE: u64 = 0;
const POST_RUNSTOP_PROBE_DELAY_SECS: u64 = 8;
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
static mut DATA_PHASE_PENDING_START: bool = false;
static mut DATA_PHASE_PENDING_LEN: usize = 0;
/// CNTPCT tick of the first successful post-connect Run/Stop (quiet-window
/// reference; 0 = no start recorded yet).
static mut RUN_STOP_TICK: u64 = 0;
/// Connect-delay one-shot latch (see the delay block in
/// `init_with_super_speed`). Only the first handoff attempt pays the delay
/// so the retry loop stays inside the EL1 recovery-timer budget.
static mut SIGNAL_CONNECT_DELAYED: bool = false;
/// Adopted SMMU mapping (see `adopt_smmu_dma_mapping()`). When the Apps-SMMU
/// stream is owned by a live TRANSLATE context that software cannot rewrite,
/// the EP0 DMA objects are relocated into a page that context already maps:
/// the CPU addresses the page at `DMA_ADOPTED_CPU` while DWC3 is published
/// the corresponding IOVA in `DMA_ADOPTED_IOVA`.
static mut DMA_ADOPTED: bool = false;
static mut DMA_ADOPTED_CPU: usize = 0;
static mut DMA_ADOPTED_IOVA: u64 = 0;

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
/// 0..2 are STARTTRANSFER, ENDTRANSFER, and event-ring delivery. The caller
/// maps no-record to 9 and a recorded bitmask to bitmask+1 so that the
/// distinction survives the pull-up attach-count channel.
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
            XBL_EP0_EVENT_SIZE
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

/// DWC3's EP0 CONTROL_SETUP transfer DMA target is the EP0 TRB itself.
/// The controller writes the eight-byte USB setup packet over the TRB's
/// first eight bytes, and Linux/Android reads it back through `dwc->ep0_trb`.
/// Keep this separate from the TRB descriptor pointer so all cache operations
/// use the same address and ownership boundary.
#[inline]
unsafe fn ep0_setup_data_ptr() -> *mut u8 {
    unsafe { ep0_trb_ptr(0).cast::<u8>() }
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
/// The gadget driver is deliberately independent of DWC3 registers.  The
/// hardware UDC feeds it setup/complete callbacks, while the QEMU simulator
/// uses the same request/state implementation directly.
static mut GADGET: Ep0Simulator = Ep0Simulator::new();
static mut UDC: UsbUdc = UsbUdc::new();

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
/// Newest post-Run/Stop event-DMA probe result. Bits 0..2 are STARTTRANSFER,
/// ENDTRANSFER, and event-ring delivery respectively; bit 16 marks that the
/// retained trace contained a probe record at all.
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
    }
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

/// Restore the captured GCTL.RAMCLKSEL. Called after CSFTRST and after the
/// host's bus USB reset, both of which clear the field.
unsafe fn reapply_ramclksel() {
    unsafe {
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
                // The post probe stores started/ended in value/index and
                // the first event word in length. A nonzero event word is
                // the observed delivery marker; the record itself lets the
                // gate distinguish "probe not compiled/reached" from a
                // probe that ran and failed one of its checks.
                let started = read_volatile(addr_of!((*entry).value)) & 1;
                let ended = read_volatile(addr_of!((*entry).index)) & 1;
                let delivered = (read_volatile(addr_of!((*entry).length)) >> 31) & 1;
                TRACE_HARVEST_POST = 0x1_0000 | started | (ended << 1) | (delivered << 2);
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
        let gdsc = core::ptr::read_volatile(
            super::platform::bramble::usb_resources().gdsc as *const u32,
        );
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

pub fn utmi_readout_code(selector: &str) -> u32 {
    if selector == "protocol" {
        return protocol_readout_code();
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
        "ss-speed" | "ss-link" | "ss-pipe" | "ss-gctl" | "ss-qmp" | "ss-vbus"
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
                "ss-pipe" => (u32::from(pipe & GUSB3PIPECTL_SUSPHY != 0))
                    | (u32::from(pipe & GUSB3PIPECTL_PHYSOFTRST != 0) << 1)
                    | (u32::from(dctl & DCTL_RUN_STOP != 0) << 2)
                    | (u32::from(dsts & DSTS_DEVCTRLHLT != 0) << 3),
                // GCTL.PRTCAPDIR plus the two-bit RAMCLKSEL value.
                "ss-gctl" => ((gctl & GCTL_PRTCAPDIR_MASK) >> 12)
                    | (gctl_ramclksel(gctl) << 2),
                // QMP PHYSTATUS, PCS_START_CONTROL[1:0], and TYPEC_CTRL[0].
                "ss-qmp" => u32::from(qmp & QMP_PHYSTATUS != 0)
                    | (((qmp >> 16) & 0x3) << 1)
                    | (((qmp >> 24) & 0x1) << 3),
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
                        | (u32::from(post_dsts != 0xffff_ffff
                            && post_dsts & DSTS_DEVCTRLHLT != 0)
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
                "ss-gctl-device" => {
                    u32::from(gctl & GCTL_PRTCAPDIR_MASK == GCTL_PRTCAP_DEVICE)
                }
                "ss-qmp-phystatus" => u32::from(qmp & QMP_PHYSTATUS != 0),
                "ss-qmp-start0" => (qmp >> 16) & 1,
                "ss-qmp-start1" => (qmp >> 17) & 1,
                "ss-qmp-typec0" => (qmp >> 24) & 1,
                // QMP PCS_STATUS2[3] is the official Qualcomm
                // RX_EQUALIZATION_IN_PROGRESS indicator.
                "ss-qmp-rxeq" => {
                    u32::from(SS_STATE_SNAPSHOT_QMP_STATUS2 & (1 << 3) != 0)
                }
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
                "ss-qscratch-phystatus-sw" => {
                    u32::from(qscratch_general & (1 << 3) != 0)
                }
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
        if let Some(selector) = option_env!("FULLERENE_USB_SIGNAL_CMD_GATE")
            .filter(|value| value.starts_with("ss-"))
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
        if stage >= 13
            && stage != 20
            && cfg!(fullerene_aarch64_usb_gadget_handoff_super_speed)
        {
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

// STARTTRANSFER must DMA-fetch the TRB before the command can retire, and on
// this platform that first fetch is far slower than the other endpoint
// commands (which complete from the register path alone). The probe-era
// 5,000-read window expired before the fetch finished, so give the command a
// time-based budget instead: 5000 reads is roughly 0.5-1 ms of MMIO polling;
// 2,000,000 reads bounds the wait at a comfortable fraction of a second
// without ever spinning forever.
const DWC3_EP_COMMAND_TIMEOUT: u32 = 50_000;

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

/// True when the address lives in the Device-mapped 2 MiB block that holds
/// the .usb_dma/.usb_trace sections: the CPU accesses it uncached, so the
/// DC maintenance is unnecessary (and constrained-unpredictable on Device
/// memory) — skip it.
#[inline]
fn in_uncached_dma_window(address: usize) -> bool {
    let dma_start = addr_of!(__usb_dma_start) as usize;
    let dma_end = addr_of!(__usb_dma_end) as usize;
    let trace_start = addr_of!(__usb_trace_start) as usize;
    let trace_end = addr_of!(__usb_trace_end) as usize;
    let dma_valid = dma_start != 0 && dma_end > dma_start;
    let trace_valid = trace_start != 0 && trace_end > trace_start;
    let Some((section_start, section_end)) = (match (dma_valid, trace_valid) {
        (true, true) => Some((dma_start.min(trace_start), dma_end.max(trace_end))),
        (true, false) => Some((dma_start, dma_end)),
        (false, true) => Some((trace_start, trace_end)),
        (false, false) => None,
    }) else {
        return false;
    };
    let block_base = section_start & !0x1_fFFF;
    let block_top = (section_end.saturating_sub(1) & !0x1_fFFF) + 0x20_0000;
    address >= block_base && address < block_top
}

unsafe fn cache_clean(address: usize, length: usize) {
    // DWC3 and the Apps SMMU consume these objects by DMA.  The probe may be
    // entered with the bootloader's caches enabled, so a no-op here would
    // leave the freshly written TRB/page table only in the CPU cache.
    if !super::platform::bramble::usb_resources()
        .gsi
        .disable_io_coherency
    {
        unsafe { core::arch::asm!("dsb sy", options(nostack)) };
        return;
    }
    if in_uncached_dma_window(address) {
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
    if !super::platform::bramble::usb_resources()
        .gsi
        .disable_io_coherency
    {
        unsafe { core::arch::asm!("dsb sy", options(nostack)) };
        return;
    }
    if in_uncached_dma_window(address) {
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
        if command_kind == DEPCMD_ENDTRANSFER
            || read(DSTS) & DSTS_CONNECTSPD_MASK != DSTS_SUPERSPEED
        {
            let mut usb2 = read(GUSB2PHYCFG0);
            saved_usb2_config = usb2 & (GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
            if saved_usb2_config != 0 {
                usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
                write(GUSB2PHYCFG0, usb2);
                let _ = read(GUSB2PHYCFG0);
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
    // Linux's dwc3_send_gadget_ep_cmd() uses a bounded 5,000-read polling
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
            if saved_usb2_config != 0 {
                unsafe {
                    let usb2 = read(GUSB2PHYCFG0);
                    write(GUSB2PHYCFG0, usb2 | saved_usb2_config);
                }
            }
            return success.then_some(resource_index);
        }
        unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
    }
    trace_event(
        TRACE_EP_COMMAND_TIMEOUT,
        command,
        endpoint as u32,
        unsafe { read(dep_reg(endpoint, 0x0c)) },
        0,
        unsafe { read(DSTS) },
    );
    if saved_usb2_config != 0 {
        unsafe {
            let usb2 = read(GUSB2PHYCFG0);
            write(GUSB2PHYCFG0, usb2 | saved_usb2_config);
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
    let abl_ep_config = cfg!(fullerene_aarch64_usb_abl_ep_config)
        && endpoint <= 1
        && !modify;
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
        write_volatile(
            addr_of_mut!((*trb).ctrl),
            kind | flags,
        );
        cache_clean(trb as usize, core::mem::size_of::<Trb>());
    }
}

/// Prepare the EP0 SETUP TRB. DWC3's CONTROL_SETUP buffer pointer is the
/// ring entry's own address: the controller writes the setup packet into the
/// first eight bytes of that TRB, which is also how Android msm reads it.
unsafe fn prepare_ep0_setup_trb() {
    let kind = if cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup) {
        TRB_XBL_EP0_SETUP
    } else {
        TRB_CONTROL_SETUP
    };
    unsafe { prepare_trb(0, ep0_setup_data_ptr() as *const u8, 8, kind) };
}

unsafe fn prepare_trb_at(trb: *mut Trb, buffer: *const u8, length: usize, kind: u32) {
    let address = unsafe { dma_iova_for(buffer as usize) };
    unsafe {
        write_volatile(addr_of_mut!((*trb).bpl), address as u32);
        write_volatile(addr_of_mut!((*trb).bph), (address >> 32) as u32);
        write_volatile(addr_of_mut!((*trb).size), length as u32);
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

/// Exercise the live EP0 event-DMA path after Run/Stop without issuing a
/// second STARTTRANSFER on an endpoint that already owns the real SETUP
/// transfer. DWC3 rejects that overlap as No Resource. End the live request,
/// consume its command/event record, run the synthetic start/end pair, then
/// leave the caller to restore the host-facing SETUP transfer.
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
unsafe fn post_runstop_event_dma_probe() -> bool {
    unsafe {
        let active_resource = EP0_RESOURCE_INDEX[0];
        if EP0_SETUP_ARMED || active_resource != 0 {
            let resource = active_resource.max(1);
            if !send_ep_command(
                0,
                DEPCMD_ENDTRANSFER
                    | DEPCMD_CMDIOC
                    | DEPCMD_HIPRI_FORCERM
                    | ((resource as u32) << DEPCMD_PARAM_SHIFT),
                0,
                0,
                0,
            ) {
                log_puts("usb: post-Run/Stop probe could not end live SETUP\n");
                return false;
            }
            EP0_RESOURCE_INDEX[0] = 0;
            EP0_SETUP_ARMED = false;
            // Consume the live SETUP ENDTRANSFER completion before sampling
            // the synthetic request; otherwise the first GEVNTCOUNT word
            // would describe the teardown rather than the test pair.
            let _ = poll_ep0_event_ring();
        }

        let link_ready = read(DSTS) & DSTS_DEVCTRLHLT == 0
            && read(DCTL) & DCTL_RUN_STOP != 0;
        let started = if link_ready {
            prepare_ep0_setup_trb();
            start_transfer(0, ep0_trb_ptr(0))
        } else {
            false
        };
        let resource = if started {
            EP0_RESOURCE_INDEX[0].max(1)
        } else {
            1
        };
        let ended = send_ep_command(
            0,
            DEPCMD_ENDTRANSFER
                | DEPCMD_CMDIOC
                | DEPCMD_HIPRI_FORCERM
                | ((resource as u32) << DEPCMD_PARAM_SHIFT),
            0,
            0,
            0,
        );
        EP0_RESOURCE_INDEX[0] = 0;
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
            started as u32,
            ended as u32,
            event_word | ((delivered as u32) << 31),
            read(DSTS),
        );
        if !started || !ended || !delivered {
            trace_marker(TRACE_PROBE_WATCHDOG, 0x504F_5346); // "POSF"
            log_puts("usb: post-Run/Stop event DMA probe failed\n");
            return false;
        }
        EVENT_OFFSET = 0;
        prepare_ep0_setup_trb();
        // ENDTRANSFER retires the synthetic probe request. Restore the real
        // EP0 OUT SETUP transfer before returning to host enumeration.
        if !start_transfer(0, ep0_trb_ptr(0)) {
            log_puts("usb: post-Run/Stop probe SETUP re-arm failed\n");
            return false;
        }
        EP0_SETUP_ARMED = true;
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
            EP0_SETUP_ARMED = true;
            PENDING_SETUP_ARM = false;
            trace_event(TRACE_SETUP_QUEUED, 0x4152_4D45, 0, 0, 0, read(DSTS)); // "ARME"
            true
        } else {
            // Fast-fail ("No resource" completes immediately). The host's
            // first SETUP token lands ~1 ms after its bus reset ends, so the
            // retry rate must place an armed SETUP TRB inside that window
            // while still bounding the total failed-command count.
            // BISECT: the 20-poll cooldown variant never reached the data
            // phase (-110 in every run); the 200-poll cooldown is the value
            // from the era when the device answered with a data-phase -71.
            ARM_COOLDOWN = 200;
            false
        }
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
        // The host's bus USB reset clears GCTL.RAMCLKSEL (the Linux comment
        // about reprogramming it on Connect Done documents exactly this);
        // restore the captured working select before the EP0 rebuild.
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
        GadgetDriver::reset(gadget_mut());
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
            // The protocol layer exposes one vendor function with either an
            // ordinary bulk pair or an explicitly supplied IPA/GSI binding.
            // Configure it only after SET_CONFIGURATION has committed,
            // matching gadget-core ordering.
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
                        MAX_PACKET_SIZE as usize,
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
    #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
    unsafe {
        // The SETUP buffer is cleared immediately below so a later poll can
        // distinguish a fresh packet. Preserve a diagnostic latch before
        // clearing it; otherwise the host-visible early-drop probe can miss
        // a real DMA delivery between two 1 ms samples.
        SIGNAL_SETUP_PACKET_RECEIVED = true;
    }
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
                trace_utmi_state(6);
                trace_event(TRACE_USB_RESET, 0, 0, 0, 0, raw);
                note_runtime_event(super::platform::bramble::UsbRuntimeEvent::BusReset);
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
                unsafe { configure_android_hs_connect_done_policy(speed) };
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
            let mut fresh_setup = false;
            for offset in 0..8 {
                if read_volatile(setup.add(offset)) != 0 {
                    fresh_setup = true;
                    break;
                }
            }
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
                MAX_PACKET_SIZE as usize,
            );
        }
    }
}

/// Initialize the Bramble DWC3 in peripheral mode and connect the pull-up.
pub fn init() -> bool {
    init_with_super_speed(true, true, true)
}

/// Initialize only the USB2 path for the dependency-free hardware probe.
pub fn init_usb2_only() -> bool {
    init_with_super_speed(false, true, true)
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
    let mask = unsafe { read_qscratch(QSCRATCH_PWR_EVENT_MASK) }
        | PWR_EVENT_POWERDOWN_IN_P3
        | PWR_EVENT_POWERDOWN_OUT_P3
        | PWR_EVENT_LPM_OUT_L1;
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
/// preserve the previous state until a link-state read is available rather
/// than guessing and changing the platform vote spuriously.
#[inline]
unsafe fn update_p3_state(status: u32) {
    let p3_in = status & PWR_EVENT_POWERDOWN_IN_P3 != 0;
    let p3_out = status & PWR_EVENT_POWERDOWN_OUT_P3 != 0;
    if p3_in && !p3_out {
        USB_IN_P3 = true;
    } else if p3_out && !p3_in {
        USB_IN_P3 = false;
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
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(QSCRATCH_HS_PHY_CTRL, (1 << 20) | (1 << 28));
        enable_power_events();
        if QMP_PHY_READY {
            qmp_set_autonomous_mode(false);
        }
        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
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
                MAX_PACKET_SIZE as usize,
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
        let performance =
            super::platform::bramble::usb_performance_state(performance_vote);
        if !super::platform::bramble::apply_usb_power(true, false) {
            log_puts("usb: RPMh USB PHY regulator vote unavailable; continuing\n");
        }
        let _ = super::platform::bramble::enable_usb30_gdsc();
        let _ = super::platform::bramble::apply_usb_performance(performance.vote);
        let _ = super::platform::bramble::usb_bus_vectors(performance.vote);
    }

    #[cfg(all(
        fullerene_aarch64_usb_gadget_handoff_probe,
        not(fullerene_aarch64_usb_gadget_handoff_super_speed)
    ))]
    {
        if init_usb2_gadget_handoff() {
            return true;
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

    if init_with_super_speed(false, true, false) {
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

        // This probe also resets the controller, so mirror the Qualcomm
        // post-reset callback before asking Run/Stop to reconnect.
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
        // Match dwc3_qcom_vbus_override_enable(): the Qualcomm glue asserts
        // both the SuperSpeed lane power-present vote and the USB2
        // VBUS/session override, even when the gadget is intentionally
        // limited to USB2.  Writing only HS_PHY_CTRL leaves the role change
        // incomplete on platforms whose Type-C glue gates the pull-up with
        // the SS-side vote.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24); // LANE0_PWR_PRESENT
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        qscratch_set(QSCRATCH_CGCTL, 0x18);
        enable_power_events();
        // Fastboot may leave the core in the USB2 suspended state when it
        // tears down its gadget just before jumping to the temporary image.
        // Waking the UTMI block is still below the EP0/DMA boundary and is
        // required before DCTL.Run/Stop can produce a new pull-up.
        let mut usb2 = read_volatile(reg(GUSB2PHYCFG0));
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        write_volatile(reg(GUSB2PHYCFG0), usb2);
        let mut usb3 = read_volatile(reg(GUSB3PIPECTL0));
        usb3 |= GUSB3PIPECTL_SUSPHY;
        write_volatile(reg(GUSB3PIPECTL0), usb3);
        let general = read_volatile(qscratch_reg(QSCRATCH_GENERAL_CFG));
        write_volatile(
            qscratch_reg(QSCRATCH_GENERAL_CFG),
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

        let gctl = read_volatile(reg(GCTL));
        write_volatile(
            reg(GCTL),
            (gctl & !GCTL_PRTCAPDIR_MASK) | GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG,
        );
        configure_dwc3_global_control();
        write_volatile(reg(DCFG), DCFG_HIGHSPEED);
        // Linux disables endpoint advertising before stopping the device
        // controller. In the ABL Stop() differential, the controller remains
        // live, so leave its endpoint advertisement untouched until the
        // later handoff sequence explicitly clears it.
        if connect || !cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_runstop) {
            write_volatile(reg(DALEPENA), if connect { 0b11 } else { 0 });
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

        // Qualcomm's glue reasserts the VBUS override immediately before
        // enabling RUN_STOP so a stale Fastboot session cannot suppress the
        // connect-done transition.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
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

#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
unsafe fn init_usb2_gadget_reuse_fastboot_ep0() -> bool {
    // Re-establish the Qualcomm PHY/session state without asserting the
    // pull-up yet.  EP0 must be fully configured, its event ring published,
    // and the first SETUP TRB armed before Run/Stop is allowed to advertise
    // the device; otherwise the host can issue the first descriptor request
    // while the handoff is still rebuilding DWC3 state.
    // This USB2 entry point bypasses `init_with_super_speed`; apply the same
    // explicit SMMU-disable differential here before publishing any new EP0
    // DMA object. Previously `--smmu-disable` was ignored on this path.
    if !prepare_smmu_dma_bypass() {
        return false;
    }
    // Snapshot the Fastboot-owned clock/PHY state before the handoff starts
    // changing the controller-side USB2 session.
    trace_utmi_state(1);
    // Stage 1 is deliberately before even the initial stop/readback: it is
    // the control experiment against the already-proven bare pull-up path.
    if unsafe { stop_after_gadget_handoff_stage(1) } {
        return true;
    }
    unsafe { gate_flow_blip() }; // flow-map B1: reuse entry
    if cfg!(fullerene_aarch64_usb_gadget_handoff_reuse_fastboot_dma) {
        // Capture the address while Fastboot still owns the controller. The
        // no-SMMU differential deliberately preserves that firmware stream
        // mapping; changing the address to the linker section would defeat
        // this experiment before the first STARTTRANSFER.
        let event_address =
            (read_volatile(reg(GEVNTADRHI0)) as u64) << 32 | read_volatile(reg(GEVNTADRLO0)) as u64;
        if event_address == 0
            || event_address == u64::MAX
            // DWC3 event buffers require 16-byte alignment. Stock Bramble
            // XBL programs an address ending in 0x10, so requiring a page
            // boundary would reject the firmware-owned buffer and silently
            // force the caller into the non-reuse fallback path.
            || event_address & 0xf != 0
            || event_address > usize::MAX as u64
        {
            log_puts("usb gadget handoff: Fastboot event DMA address invalid\n");
            trace_event(
                TRACE_FASTBOOT_EVENT_DMA,
                event_address as u32,
                (event_address >> 32) as u32,
                0,
                0,
                read(DSTS),
            );
            return gadget_handoff_fail(1);
        }
        FASTBOOT_EVENT_DMA_BASE = event_address;
        log_hex(
            "usb gadget handoff: reusing Fastboot event DMA=",
            event_address,
        );
        trace_event(
            TRACE_FASTBOOT_EVENT_DMA,
            event_address as u32,
            (event_address >> 32) as u32,
            FASTBOOT_EP0_EVENT_SIZE as u32,
            1,
            read(DSTS),
        );
    }
    // Capture the working Fastboot RAM clock selection before the reuse
    // helper stops the old session or the device soft reset clears GCTL.
    // Zero is a valid RAMCLKSEL value, so validity is tracked separately.
    unsafe {
        RAMCLK_CAPTURE = gctl_ramclksel(read(GCTL));
        RAMCLK_CAPTURE_VALID = true;
        trace_event(
            TRACE_DWC3_REVISION_QUIRK,
            0x5243_4150,
            RAMCLK_CAPTURE,
            0,
            0,
            0,
        );
    }
    if !unsafe { init_usb2_bare_pullup_handoff_inner(false) } {
        // Fastboot can leave DSTS.DEVCTRLHLT stale while the device session
        // is already quiescent. The DWC3 device soft reset below is the real
        // endpoint-resource ownership boundary and clears that state before
        // any Fullerene TRB is published. Keep the failed stop readback in
        // the retained trace/log, but do not discard an otherwise recoverable
        // handoff before reaching the reset that Linux performs next.
        log_puts(
            "usb gadget handoff: pre-reset halt readback timed out; continuing to device reset\n",
        );
        trace_event(TRACE_DWC3_HALT_TIMEOUT, 0, 0, 0, 0, read(DSTS));
    }
    // Android's msm_hsphy_init() always calls msm_hsphy_enable_power(true)
    // before enabling the 19.2 MHz ref clock, asserting PHY reset, and
    // programming the analog registers. The normal non-destructive path
    // preserves Fastboot's rails; this opt-in A/B re-sends the exact three
    // QUSB2 rail enable requests before the direct reset/init boundary
    // without changing the DWC3 command or response path.
    #[cfg(fullerene_aarch64_usb_refresh_hsphy_power)]
    if !unsafe { super::platform::bramble::refresh_usb_power(false) } {
        log_puts("usb gadget handoff: HS PHY rail refresh failed\n");
        trace_event(TRACE_USB2_PHY_RESET, 0x5057_5246, 0, 0, 0, read(DSTS)); // "PWRF"
    }
    #[cfg(fullerene_aarch64_usb_android_block_reset)]
    if !unsafe { super::platform::bramble::android_controller_block_reset() } {
        log_puts("usb gadget handoff: Android controller block reset failed\n");
        return gadget_handoff_fail(2);
    }
    #[cfg(fullerene_aarch64_usb_android_block_reset)]
    trace_event(TRACE_DWC3_RESET_BEGIN, 0x424C_4B52, 0, 0, 0, 0); // "BLKR"
    // Fastboot may have stopped Run/Stop, but that is not the same ownership
    // boundary as Linux's DWC3 probe.  The default path terminates its
    // endpoint-resource epoch with a device core soft reset.  The explicit
    // preserve-core differential keeps that reset out of the experiment while
    // retaining the preceding halted-controller boundary; this tests whether
    // the reset itself destroys the live Qualcomm PHY/session handoff.
    if !cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_core) {
        if !unsafe { device_soft_reset() } {
            log_puts("usb gadget handoff: DWC3 device reset failed\n");
            return gadget_handoff_fail(2); // core reset
        }
    } else {
        trace_marker(TRACE_DWC3_RESET_BEGIN, 0x50524553); // "PRES"
        log_puts("usb gadget handoff: preserving DWC3 core state\n");
    }
    unsafe { gate_flow_blip() }; // flow-map B2: core reset done
    if unsafe { stop_after_gadget_handoff_stage(2) } {
        return false;
    }
    // 4.19 resume order: utmi_clk is enabled after core_clk and before any
    // controller start.  The SS-only fastboot session never raised the mock
    // UTMI branch, so bring it up at this post-reset boundary; the core
    // branch is already running under firmware.
    if cfg!(fullerene_aarch64_usb_gadget_handoff_clock_branches_rearm)
        && !unsafe { super::platform::bramble::rearm_usb2_android_clock_branches() }
    {
        log_puts("usb gadget handoff: Android controller clock branch rearm failed\n");
    }
    if !unsafe { super::platform::bramble::enable_usb_hs_phy_ref_clock() } {
        log_puts("usb gadget handoff: RPMh HS PHY ref clock enable failed\n");
    }
    if !unsafe { super::platform::bramble::enable_usb2_utmi_clock() } {
        log_puts("usb gadget handoff: GCC mock UTMI clock enable failed\n");
        trace_event(TRACE_GCC_UTMI_CLOCK, 0, 0, 0, 0, read(DSTS));
    }
    trace_utmi_state(2);
    let clock_delay_us = usb_clock_stable_delay_us();
    if clock_delay_us != 0 {
        log_hex(
            "usb gadget handoff: clock stabilization delay us=",
            clock_delay_us as u64,
        );
        crate::timer::delay_us(clock_delay_us as u64);
        trace_utmi_state(8);
    }
    unsafe { configure_dwc3_global_control() };
    // The halted-controller boundary above transfers DMA ownership from the
    // old Fastboot session.  Clear every linker-owned TRB/event/table object
    // only after that boundary, then seed the allocator used by a later
    // GSI/UDC bind; clearing it before the stop could race a final bootloader
    // DMA write.
    clear_dma_memory();
    unsafe {
        // msm-4.19 dwc3_core_setup_global_control() end state for this
        // core: device port, SCALEDOWN off, clock gating disabled (lito DT
        // snps,disable-clk-gating), hibernation only on HIB power-option
        // cores. The previous code preserved whatever SCALEDOWN state the
        // bootloader left behind.
        let mut gctl = read(GCTL);
        gctl &= !(GCTL_PRTCAPDIR_MASK | GCTL_SCALEDOWN_MASK);
        gctl |= GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG;
        if (read(GHWPARAMS1) & GHWPARAMS1_EN_PWROPT_MASK) == GHWPARAMS1_EN_PWROPT_HIB {
            gctl |= GCTL_GBLHIBERNATIONEN;
        }
        write(GCTL, gctl);
        // The reset may clear RAMCLKSEL even when the surrounding GCTL bits
        // are preserved. Restore the Fastboot value before endpoint setup.
        reapply_ramclksel();
        // CSFTRST restores the controller-side PHY mux/timing state on
        // DWC3 revisions used by Bramble. Reapply the Qualcomm controller
        // programming before any endpoint command. In the preserve-core
        // differential these writes are deliberately retained as the common
        // post-halt handoff sequence; only CSFTRST itself is omitted.
        // The Qualcomm msm glue selects UTMI-as-PIPE and then updates the
        // DWC3 reference-clock period. The direct Fastboot reuse path must
        // keep that callback too: an electrical attach can succeed while
        // stale REFCLKPER/GFLADJ leaves UTMI packet timing unusable.
        select_utmi_pipe_clock();
        update_dwc3_ref_clock();
        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        write(GUSB2PHYCFG0, usb2);
        let mut usb3 = read(GUSB3PIPECTL0);
        usb3 |= GUSB3PIPECTL_SUSPHY;
        write(GUSB3PIPECTL0, usb3);
        // The generic GCTL path above early-returns on this DWC_usb31 core,
        // so the 4.19 usb31 reference state is applied at this post-reset
        // boundary instead: GUSB2PHYCFG UTMI timing (dwc3_hs_phy_setup
        // steady state) plus the GUCTL1/GUCTL3/GSBUSCFG1 bits from
        // setup_global_control and __dwc3_gadget_start.
        configure_usb2_phy_interface();
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_post_endpoint_global) {
            apply_usb31_gadget_reference_deltas();
        }
        // Android msm's dwc3_set_mode(DEVICE) performs a second GCTL write
        // after selecting the device port. Preserve that controller-mode
        // tail before publishing EP0 resources; it is independent of TRB
        // payload framing and is especially relevant on DWC_usb31.
        configure_dwc3_device_mode();
        }
        // An SS-only fastboot session never deasserted the femto PHY block
    // reset (GCC_QUSB2PHY_PRIM_BCR), which can leave the PHY core logic held
    // in reset while the D+/D- IO state machine still answers the host reset
    // autonomously.  The 4.19 phy-core deasserts `phy_reset` before
    // `snps_hsphy_init`; pulse the USB2-only line here, before the pull-up
    // is asserted, so the host port stays unattached throughout.
    if !cfg!(fullerene_aarch64_usb_skip_usb2_phy_reset) {
        if !unsafe { super::platform::bramble::pulse_usb2_phy_reset() } {
            log_puts("usb gadget handoff: USB2 PHY BCR reset failed\n");
            trace_event(TRACE_USB2_PHY_RESET, 0, 0, 0, 0, read(DSTS));
        }
    } else {
        log_puts("usb gadget handoff: skipping USB2 PHY BCR reset A/B\n");
        trace_event(TRACE_USB2_PHY_RESET, 0x534B_4950, 0, 0, 0, read(DSTS));
    }
    // DWC3's device reset does not reset the external Femto PHY.  Reapply the
    // Linux USB2 PHY programming at the same post-reset boundary as the normal
    // Qualcomm glue path; the GCC/Type-C power-domain (core) reset stays
    // untouched, only the USB2 PHY BCR line above is pulsed.
    if !cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_core) {
        unsafe { init_hsphy() };
    }
    // The external QUSB2 PHY reset/init above can restore the DWC3-side USB2
    // interface register to its reset value.  Re-apply the UTMI contract
    // after that boundary, immediately before endpoint resources are built;
    // otherwise the source-level TRDTIM=9 write made before init_hsphy() is
    // absent from the actual stage-3 readback and the core advertises a
    // pull-up without a usable USB2 transaction interface.
        unsafe { configure_usb2_phy_interface() };
    trace_utmi_state(3);

    // The Fastboot session may have left the DWC3 stream behind an SMMU
    // mapping that only covers its own buffers.  Our TRBs/event ring are
    // intentionally identity-addressed in the 0x9b800000 DMA section.  Keep
    // the proven PHY/pull-up transition first, then install the identity map
    // before handing any new DMA object to DWC3.
    let smmu_ready = if cfg!(fullerene_aarch64_usb_gadget_handoff_no_smmu) {
        // Differential mode for a Fastboot-owned bypass: do not even read
        // the Apps-SMMU registers. The DMA section remains fixed inside the
        // declared Bramble pool, so this mode is valid only when firmware
        // leaves the DWC3 stream in physical=IOVA bypass.
        log_puts("usb gadget handoff: Apps SMMU untouched\n");
        true
    } else {
        configure_dwc3_smmu()
    };
    if !smmu_ready {
        log_puts("usb gadget handoff: DWC3 SMMU pool map unavailable\n");
        return gadget_handoff_fail(3); // SMMU
    }
    unsafe { gate_flow_blip() }; // flow-map B3: SMMU ready
    if unsafe { stop_after_gadget_handoff_stage(3) } {
        return false;
    }

    let event_address = unsafe { ep0_event_address() };
    unsafe {
        // Reusing the bootloader's DMA context must not expose stale event
        // words from the previous Fastboot session to the polled consumer.
        let event_size = ep0_event_size();
        let event_words = ep0_event_dma_base() as *mut u32;
        for index in 0..(event_size / core::mem::size_of::<u32>()) {
            write_volatile(event_words.add(index), 0);
        }
        core::ptr::write_bytes(
            ep0_trb_ptr(0).cast::<u8>(),
            0,
            2 * core::mem::size_of::<Trb>(),
        );
        core::ptr::write_bytes(ep0_response_ptr(), 0, 512);
        cache_clean(ep0_event_dma_base(), event_size);
        cache_clean(ep0_trb_ptr(0) as usize, 2 * core::mem::size_of::<Trb>());
        cache_clean(ep0_response_ptr() as usize, 512);
        write(GEVNTADRLO0, event_address as u32);
        write(GEVNTADRHI0, (event_address >> 32) as u32);
        write(GEVNTSIZ0, event_size as u32);
        acknowledge_ep0_event_count();
        trace_event(
            TRACE_EVENT_RING_READY,
            event_address as u32,
            (event_address >> 32) as u32,
            event_size as u32,
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
        EP0_SETUP_ARMED = false;
        CONFIGURED = false;
        DATA_ENDPOINTS_READY = false;
        DATA_REQUEST_SLOTS = [usize::MAX; 2];
        DATA_RESOURCE_INDEX = [0; 2];
        EP0_RESOURCE_INDEX = [0; 2];
        GSI_GADGET_BOUND = false;
        FUNCTION_BOUND = false;
        // The core reset above invalidates Fastboot's endpoint configuration
        // and transfer resources. Rebuild both control directions from the
        // INIT state; this is the same ownership boundary used by Linux.
        ENDPOINTS_READY = false;
        // Fastboot's handoff requires a known USB2 device-mode speed while
        // the endpoint resources are rebuilt. The final Run/Stop boundary
        // still reapplies the old-DWC3 speed workaround immediately before
        // connection, but leaving this intermediate state unspecified loses
        // the physical attach on Bramble.
        write(DCFG, if cfg!(fullerene_aarch64_usb_dcfg_superspeed) {
            DCFG_SUPERSPEED
        } else {
            DCFG_HIGHSPEED
        });
        configure_gadget_start_defaults();
        // Linux enables each endpoint only after its SETEPCONFIG and
        // SETTRANSFRESOURCE commands complete. Do not advertise EP0 before
        // the controller has accepted the corresponding resource state.
        write(DALEPENA, 0);
        // DEPSTARTCFG(0) opens a new endpoint-resource allocation window.
        // SETEPCONFIG(INIT) then allocates one resource per EP0 direction.
        if !send_ep_command(0, DEPCMD_DEPSTARTCFG, 0, 0, 0) {
            log_puts("usb gadget handoff: DEPSTARTCFG failed\n");
            return gadget_handoff_fail(4); // resource window
        }
        if stop_after_gadget_handoff_stage(4) {
            return false;
        }
        // Android's msm DWC3 glue allocates transfer resources for the
        // available endpoints immediately after DEPSTARTCFG, before issuing
        // SETEPCONFIG. Keep this ordering as an explicit Bramble differential;
        // the upstream Linux ordering remains the default path elsewhere.
        if cfg!(fullerene_aarch64_usb_gadget_handoff_android_resource_order)
            && !cfg!(fullerene_aarch64_usb_gadget_handoff_no_transfer_resource)
        {
            // Android's msm driver walks dwc->eps[] rather than only the
            // endpoints that the current gadget will expose. The msm-4.19
            // implementation loops over ALL DWC3_ENDPOINTS_NUM (32) hardware
            // endpoints right after DEPSTARTCFG and before any SETEPCONFIG,
            // so mirror that exactly instead of trusting a GHWPARAMS3 field
            // encoding that may not match this core.
            for endpoint in 0..32u32 {
                if !set_transfer_resource(endpoint as usize) {
                    log_puts("usb gadget handoff: Android resource preallocation failed\n");
                    return gadget_handoff_fail(5); // resource allocation
                }
            }
        }
        // The direct reuse entry has already selected DCFG High-Speed. Keep
        // EP0 at the USB2 maximum packet size unless the explicit
        // Linux/Android initial-512 A/B is requested; using the SuperSpeed
        // value unconditionally leaves a High-Speed core with a mismatched
        // control context before Connect Done can modify it.
        let ep0_packet_size = if cfg!(fullerene_aarch64_usb_ep0_initial_512) {
            INITIAL_EP0_MAX_PACKET_SIZE
        } else {
            64
        };
        if !unsafe {
            configure_endpoint_config(
                0,
                ep0_packet_size,
                DEPCFG_EP_TYPE_CONTROL,
                false,
                0,
            )
        } {
            log_puts("usb gadget handoff: USB2 EP0 OUT configure failed\n");
            return gadget_handoff_fail(5); // EP0 config
        }
        if stop_after_gadget_handoff_stage(9) {
            return false;
        }
        // This path intentionally uses the config-only helper above so the
        // Qualcomm sequence remains a single SETEPCONFIG ->
        // SETTRANSFRESOURCE pair for EP0 OUT. The outer allocation is
        // required here; unlike configure_endpoint(), the config-only helper
        // does not allocate the transfer resource itself.
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_no_transfer_resource)
            && !cfg!(fullerene_aarch64_usb_gadget_handoff_android_resource_order)
            && !set_transfer_resource(0)
        {
            log_puts("usb gadget handoff: USB2 EP0 OUT resource failed\n");
            return gadget_handoff_fail(5); // EP0 resource
        }
        if stop_after_gadget_handoff_stage(10) {
            return false;
        }
        // XBL's DwcConfigureEP publishes the corresponding DALEPENA bit
        // after each SETEPCONFIG -> SETTRANSFRESOURCE pair.
        write(DALEPENA, read(DALEPENA) | (1 << 0));
        trace::live_dalepena_config(0, read(DALEPENA));
        #[cfg(fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0)]
        {
            // Stock XBL inserts the initial EP0 OUT request immediately after
            // the OUT pair and before configuring the EP0 IN direction. Keep
            // this as a separate ordering A/B; the explicit setup payload is
            // retained because the earlier XBL zero/self-buffer tests did not
            // attach on this handoff path.
            if !queue_xbl_setup_request() {
                log_puts("usb gadget handoff: XBL inter-pair request queue failed\n");
                return gadget_handoff_fail(5);
            }
            prepare_ep0_setup_trb();
            if !start_transfer(0, ep0_trb_ptr(0)) {
                log_puts("usb gadget handoff: XBL inter-pair SETUP STARTTRANSFER failed\n");
                return gadget_handoff_fail(12);
            }
            let slot = EP0_SETUP_REQUEST_SLOT;
            if slot == usize::MAX || !udc_mut().start(0, slot) {
                log_puts("usb gadget handoff: XBL inter-pair request start failed\n");
                return gadget_handoff_fail(5);
            }
            EP0_SETUP_ARMED = true;
        }
        // Stage 8 isolates the first SETEPCONFIG/SETTRANSFRESOURCE pair from
        // the corresponding EP0 IN pair. It is intentionally appended to the
        // original 1..7 sequence so existing stage numbers remain stable.
        if stop_after_gadget_handoff_stage(8) {
            return false;
        }
        if !configure_endpoint(1, ep0_packet_size, false) {
            log_puts("usb gadget handoff: USB2 EP0 configure failed\n");
            return gadget_handoff_fail(5); // EP0 config
        }
        // XBL's DwcConfigureEP publishes the corresponding DALEPENA bit
        // after each SETEPCONFIG -> SETTRANSFRESOURCE pair. Keep the two
        // physical EP0 directions on the same per-direction boundary.
        write(DALEPENA, read(DALEPENA) | (1 << 1));
        trace::live_dalepena_config(1, read(DALEPENA));
        // Stock XBL writes exactly 0x47 here: Disconnect, USB Reset, Connect
        // Done, and Suspend. Keep the narrower mask limited to the
        // event-driven XBL differential; the generic path retains its
        // broader lifecycle notifications.
        let devten = if cfg!(any(
            fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup,
            fullerene_aarch64_usb_abl_devten
        )) {
            // Factory ABL publishes exactly 0x47 here: Disconnect, USB Reset,
            // Connect Done, and Suspend. Keep this opt-in so the normal path
            // remains unchanged while event ownership is isolated.
            DEVTEN_DISCONNECT | DEVTEN_USB_RESET | DEVTEN_CONNECT_DONE | DEVTEN_SUSPEND
        } else {
            DEVTEN_DISCONNECT
                | DEVTEN_USB_RESET
                | DEVTEN_CONNECT_DONE
                | DEVTEN_LINK_STATUS_CHANGE
                | DEVTEN_WAKEUP
                | DEVTEN_HIBERNATION_REQUEST
                | DEVTEN_SUSPEND
        };
        write(DEVTEN, devten);
        #[cfg(fullerene_aarch64_usb_gadget_handoff_xbl_post_endpoint_global)]
        {
            // Stock XBL applies the usb31 global deltas only after both EP0
            // SETEPCONFIG -> SETTRANSFRESOURCE pairs and after DEVTEN /
            // DALEPENA publication. Keep this register-order differential
            // isolated from the endpoint/request A/Bs.
            apply_usb31_gadget_reference_deltas();
        }
        ENDPOINTS_READY = true;
        let _ = udc_mut().configure_endpoint(0, 64, false);
        let _ = udc_mut().configure_endpoint(1, 64, false);
        if cfg!(any(
            fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup,
            fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0
        )) {
            // Stock XBL queues this request before Run/Stop, after both EP0
            // directions have their transfer resources.
            if !queue_xbl_setup_request() {
                log_puts("usb gadget handoff: XBL EP0 request queue failed\n");
                return gadget_handoff_fail(5);
            }
        }
        if stop_after_gadget_handoff_stage(5) {
            return false;
        }
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0) {
            trace_event(TRACE_SETUP_QUEUED, 0, 0, 0, 8, read(DSTS));
            prepare_ep0_setup_trb();
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_ep0_stall_flush)]
        {
            // Keep the stall-flush differential effective when the proven
            // Fastboot-reuse handoff is selected as the primary path too.
            let _ = send_ep_command(0, DEPCMD_SETSTALL, 0, 0, 0);
            trace_event(
                TRACE_SETUP_QUEUED,
                0x5354_4C46, // "STLF"
                0,
                0,
                0,
                read(DSTS),
            );
        }
        apply_ep0_txfifo_fix();
        // Stage 11 isolates the cache-cleaned SETUP buffer/TRB publication
        // from the DWC3 STARTTRANSFER command itself. The old stage 6
        // combined both operations, so a failure there could not tell us
        // whether the DMA object or the command latch was the boundary.
        if stop_after_gadget_handoff_stage(11) {
            return false;
        }
        // On Bramble, a STARTTRANSFER issued while the device is still
        // disconnected can complete with No Resource even after
        // SETTRANSFRESOURCE returned index 1. The timing A/Bs move this
        // exact command across the Run/Stop/link boundary; the default keeps
        // the historical pre-connect command for comparison.
        let defer_initial_setup = cfg!(any(
            fullerene_aarch64_usb_gadget_handoff_start_after_connect,
            fullerene_aarch64_usb_gadget_handoff_start_after_reset,
            fullerene_aarch64_usb_gadget_handoff_start_at_connect_done
        ));
        if cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0) {
            // The inter-pair XBL differential armed the setup TRB above.
        } else if defer_initial_setup {
            PENDING_SETUP_ARM = true;
        } else if !start_transfer(0, ep0_trb_ptr(0)) {
            log_puts("usb gadget handoff: SETUP STARTTRANSFER failed\n");
            return gadget_handoff_fail(12); // STARTTRANSFER
        }
        // Record the armed SETUP TRB so the USB-reset handler takes the
        // Linux-equivalent keep-the-TRB path instead of tearing it down and
        // racing the host's first post-reset SETUP token.
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0) {
            EP0_SETUP_ARMED = !defer_initial_setup;
        }
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_direct) {
            enable_gadget_controller_irq();
        }
        // Linux enables the DWC3 event interrupt immediately after arming the
        // EP0 OUT SETUP TRB. The probe owns no asynchronous IRQ path yet, so
        // drain the ring once synchronously at the same boundary. This keeps
        // an early XFER_NOT_READY/command event from waiting until after the
        // final Run/Stop transition.
        poll_ep0_event_ring();
        // The Android downstream Bramble driver leaves the USB2 PHY wake
        // bits in the state restored by the endpoint command helper here.
        // Mainline Linux later adds an explicit dwc3_enable_susphy(true),
        // but the stage-11 control experiment shows that this older Android
        // boundary is the one that still reaches the physical pull-up.
        // Stage 12 is immediately after STARTTRANSFER completion and before
        // the final VBUS/session + Run/Stop transition.
        if stop_after_gadget_handoff_stage(12) {
            return false;
        }
        if stop_after_gadget_handoff_stage(6) {
            return false;
        }

        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(QSCRATCH_HS_PHY_CTRL, (1 << 20) | (1 << 28));
        // Bramble's DT declares maximum-speed = "super-speed". The Android
        // msm start path keeps that DCFG speed even when the negotiated link
        // later falls back to USB2; the EP0 context is changed to 64 bytes by
        // Connect Done. Keep this source-derived speed choice opt-in because
        // the existing attach-reaching baseline used a USB2 DCFG value.
        configure_gadget_speed(cfg!(fullerene_aarch64_usb_dcfg_superspeed));
        #[cfg(fullerene_aarch64_usb_gadget_handoff_event_ring_at_runstop)]
        {
            // Android msm republishes the event buffer in
            // dwc3_gadget_run_stop(true), immediately before the gadget is
            // restarted and Run/Stop is asserted. Move that publication
            // boundary into the attach-reaching USB2 reuse path; the earlier
            // timing A/B only exercised the non-attaching fallback path.
            republish_ep0_event_ring_at_runstop();
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_gadget_restart_at_runstop)]
        if !restart_gadget_at_runstop() {
            // Android's restart helper is best-effort from the caller's
            // perspective: Run/Stop is still attempted, and the host-visible
            // attach distinguishes a restart failure from a PHY failure.
            log_puts("usb gadget handoff: Android gadget restart incomplete\n");
        }
        // Gate runs skip the DEVCTRLHLT readback wait (up to 2 s on a stale
        // halt) so the handoff returns to the probe inside the biter window;
        // the stale-halt case is exactly what the diag rescue re-arm fixes.
        let gate_run = option_env!("FULLERENE_USB_PROBE_SINGLE_ATTEMPT") == Some("1");
        trace_utmi_state(4);
        if let Some(selector) = option_env!("FULLERENE_USB_UTMI_PRECONNECT_READOUT") {
            // Publish the live UTMI field through the physical attach time,
            // before the host can submit the first SETUP. This short delay
            // avoids the long post-readout park, which is truncated by the
            // handset's recovery watchdog on large selector values.
            let code = utmi_readout_code(selector).min(15);
            trace_event(
                TRACE_UTMI_STATE,
                0x0400_0000 | code,
                code,
                0,
                0,
                0,
            );
            super::timer::delay_ms(u64::from(code) * 1_000);
        }
        trace::live_dalepena_before_dctl(read(DALEPENA));
        let start_readback_ok = if gate_run {
            unsafe { run_stop_device_no_readback(true) }
        } else {
            unsafe { run_stop_device(true) }
        };
        if option_env!("FULLERENE_USB_UTMI_REAPPLY_AFTER_RUNSTOP") == Some("1") {
            // Diagnostic only: Android/Linux program GUSB2PHYCFG before
            // gadget start. If the stage-3/4 value is cleared or ignored by
            // the final Run/Stop transition on this handoff, one immediate
            // post-Run/Stop write tells us whether the register can be
            // adopted at all. Do not change the normal path without the
            // explicit A/B environment flag.
            log_puts("usb gadget handoff: re-applying USB2 interface after Run/Stop\n");
            if option_env!("FULLERENE_USB_UTMI_REAPPLY_HALTED") == Some("1") {
                // Some DWC3 revisions lock GUSB2PHYCFG timing fields while
                // Run/Stop is asserted. Reapply only at a halted boundary,
                // then start the device again before the first host SETUP.
                unsafe {
                    let _ = run_stop_device(false);
                    configure_usb2_phy_interface();
                    let _ = run_stop_device(true);
                }
            } else {
                unsafe { configure_usb2_phy_interface() };
            }
        }
        trace_utmi_state(5);
        if let Some(selector) = option_env!("FULLERENE_USB_UTMI_POSTRUN_READOUT") {
            // Encode the post-Run/Stop stage in the attach timestamp. This
            // is deliberately separate from the pre-connect readout so a
            // run without the guarded re-apply can show whether Run/Stop
            // itself cleared or altered the UTMI contract.
            let code = utmi_readout_code(selector).min(15);
            trace_event(
                TRACE_UTMI_STATE,
                0x0500_0000 | code,
                code,
                0,
                0,
                0,
            );
            super::timer::delay_ms(u64::from(code) * 1_000);
        }
        if !start_readback_ok {
            // Some Fastboot/DWC3 handoffs keep DSTS.DEVCTRLHLT stale even
            // after the Run/Stop write has reached the controller. The
            // endpoint resources and first SETUP TRB are already published
            // at this point, so discarding the handoff solely because the
            // status poll did not observe the transition would hide the
            // same physical pull-up/EP0 behaviour this probe is measuring.
            // Keep the timeout in retained trace and let host traffic decide
            // whether the controller is actually usable.
            log_puts("usb gadget handoff: DWC3 RUN/STOP readback timed out; continuing\n");
            trace_event(TRACE_DWC3_HALT_TIMEOUT, 0, 0, 0, 0, read(DSTS));
        }
        // Arm the first SETUP TRB inside the handoff: the host issues its
        // first SETUP token ~100-200 ms after the pull-up rises, and an EP0
        // with no armed TRB cannot answer it (the read/64 -110 boundary).
        // Deferring the arm to the probe's poll loop was always too late;
        // retry here while the link trains to ON. On a powered, clocked core
        // a rejected Start Transfer (link not ON yet) is retryable, not a
        // wedge.
        {
            let arm_deadline =
                arch_counter().saturating_add(arch_counter_frequency().saturating_mul(400) / 1_000);
            while arch_counter() < arm_deadline && !EP0_SETUP_ARMED {
                let _ = try_arm_setup();
                poll_ep0_event_ring();
                super::timer::delay_us(200);
            }
            // Keep the arm-status readout meaningful on the direct reuse
            // path as well as on the fallback recovery path.  The latter
            // already records 0/8 in u0_arm_recovery(), but direct handoff
            // used to leave U0_ARM_STATUS at its static sentinel forever,
            // making armstat unable to distinguish a retired EP0
            // STARTTRANSFER from a command wedge.
            U0_ARM_STATUS = if EP0_SETUP_ARMED { 0 } else { 8 };
        }
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if option_env!("FULLERENE_USB_SIGNAL_DMA_POST_RUNSTOP") == Some("1") {
            // Run/Stop returns before the host's attach debounce reaches U0.
            // Let the polling owner run the test after a bounded delay that
            // is still before Bramble's first descriptor request.
            POST_RUNSTOP_PROBE_PENDING = true;
            POST_RUNSTOP_PROBE_NOT_BEFORE = arch_counter().saturating_add(
                arch_counter_frequency().saturating_mul(POST_RUNSTOP_PROBE_DELAY_SECS),
            );
        }
        unsafe { gate_flow_blip() }; // flow-map B4: final Run/Stop readback done
        if stop_after_gadget_handoff_stage(7) {
            return false;
        }
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        // The reuse handoff is the primary direct Bramble path. Keep the
        // host-visible EP0 signal channel on this path as well as on the
        // full platform-init path; otherwise --signal-early-drop silently
        // skips its observation window whenever reuse reaches Run/Stop.
        ep0_signal_early_drop_check();
        // The probe's Type-C observer establishes Powered/Attached before
        // this point, so record the same UDC-start boundary that the normal
        // Qualcomm gadget path records. If PMIC observation was unavailable
        // this is intentionally a no-op in the state machine, but it must
        // not block EP0 testing.
        note_runtime_event(super::platform::bramble::UsbRuntimeEvent::ControllerStarted);
        return true;
    }
}

/// Reuse the physical USB2 handoff, then add the minimum DWC3 gadget state
/// needed to answer USB control transfers. The PHY and Qualcomm session
/// remain untouched; this is the early Bramble handoff path and is also
/// usable as a standalone probe.
pub fn init_usb2_gadget_handoff() -> bool {
    unsafe {
        #[cfg(fullerene_aarch64_usb_gadget_handoff_super_speed)]
        // Keep the CLI's --no-core-reset differential effective for the
        // SuperSpeed handoff too.  Before this propagation the SS entry point
        // always passed `reset_core=true`, so run reports claiming to skip
        // CSFTRST were not testing that condition at all.
        return init_with_super_speed(
            true,
            !cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_core),
            false,
        );

        #[cfg(all(
            fullerene_aarch64_usb_gadget_handoff_probe,
            not(fullerene_aarch64_usb_gadget_handoff_super_speed)
        ))]
        return init_usb2_gadget_reuse_fastboot_ep0();

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

        write(DCFG, DCFG_HIGHSPEED);
        configure_gadget_start_defaults();
        write(DALEPENA, 0);
        write(
            DEVTEN,
            DEVTEN_DISCONNECT
                | DEVTEN_USB_RESET
                | DEVTEN_CONNECT_DONE
                | DEVTEN_LINK_STATUS_CHANGE
                | DEVTEN_WAKEUP
                | DEVTEN_HIBERNATION_REQUEST
                | DEVTEN_SUSPEND
                | DEVTEN_ERRATIC_ERROR
                | DEVTEN_CMD_COMPLETE
                | DEVTEN_OVERFLOW,
        );

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
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_direct) {
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

/// stall-map beacon: one host-visible DCTL.SDIS disconnect/re-attach pair
/// (see `sdisc_blips`: the QSCRATCH/VBUS session overrides are
/// host-invisible on this board, and the SDIS soft disconnect is the proven
/// visible lever). The soft disconnect is honored only while the core is
/// running with the link ON, so beacons before Run/Stop are silent by
/// design; the last visible pair in the host journal names how far the init
/// tail got, and the ~31 s PSCI return from the cfg-block park names that
/// the tail completed at all.
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
unsafe fn stall_map_beacon() {
    unsafe {
        sdisc_blips_link_on(1);
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
        super::platform::bramble::apply_usb_pm_qos(
            super::platform::bramble::UsbBusVote::Nominal,
        );
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

fn init_with_super_speed(super_speed: bool, reset_core: bool, reset_platform: bool) -> bool {
    unsafe {
        QMP_PHY_READY = false;
        // The DWC3 stream is unattributed at the Apps-SMMU (ladder 252), and
        // Qualcomm firmware commonly leaves sCR0.WACFG set to stall+queue:
        // every DWC3 DMA then hangs in the SMMU while GEVNTCOUNT keeps
        // counting the core-internal event FIFO, which masquerades as a
        // working event ring. Rewriting SMR/S2CR from non-secure state did
        // not lift the stall, so clear the whole warning configuration and
        // take the SMMU out of the path entirely. This must happen before
        // any DWC3 DMA is armed. A rejected (secure-owned) write fails the
        // attempt so the host-visible attach names the outcome.
        if !prepare_smmu_dma_bypass() {
            return false;
        }
        // Harvest the previous attempt's STARTTRANSFER outcome before this
        // attempt's DMA-region clear wipes the trace. Attempt 1 skips the
        // harvest (the previous boot's trace was destroyed by Android).
        INIT_CALLS = INIT_CALLS.wrapping_add(1);
        if INIT_CALLS > 1 {
            harvest_trace_outcome();
        }
        // Reset the adopted-mapping state on every handoff attempt: a failed
        // attempt must not leave the next attempt publishing stale objects.
        DMA_ADOPTED = false;
        DMA_ADOPTED_CPU = 0;
        DMA_ADOPTED_IOVA = 0;
        // Read the bootloader's Apps-SMMU state and event-ring IOVA while
        // Fastboot still owns the controller. When the stream sits in a live
        // TRANSLATE context that software cannot rewrite, the EP0 DMA
        // objects are relocated into a page that context already maps.
        #[cfg(fullerene_aarch64_usb_ep0_dma_adopt)]
        {
            let adopted = adopt_smmu_dma_mapping();
            trace_event(
                TRACE_SMMU_HANDOFF,
                adopted.is_some() as u32,
                0,
                0,
                0,
                read(DSTS),
            );
        }
        if !super::platform::bramble::usb_power_contract_valid(super_speed) {
            if reset_platform {
                // A cold platform start actually re-applies the contract below,
                // so an invalid contract is fatal there.
                log_puts("usb: DT power contract invalid\n");
                return false;
            }
            // The non-destructive handoff preserves the bootloader's live
            // rails/clocks and never re-applies the contract (apply_usb_power
            // below is gated on reset_platform). The rails are empirically
            // powered (the device attaches), so a contract the fastboot DT
            // does not fully expose is not fatal for the handoff.
            log_puts("usb: DT power contract incomplete; preserving firmware state\n");
        }
        INIT_STAGE = 1;
        let performance = super::platform::bramble::usb_performance_state(
            super::platform::bramble::UsbBusVote::Nominal,
        );
        let bus_vectors = super::platform::bramble::usb_bus_vectors(performance.vote);
        log_hex("usb: nominal core clock=", performance.core_rate_hz as u64);
        log_hex(
            "usb: PM QoS latency us=",
            performance.pm_qos_latency_us as u64,
        );
        log_hex("usb: interconnect paths=", bus_vectors.len() as u64);
        // The lito DT wires the USB domain's two RPMh votes outside every USB
        // node: the `gcc` block's `vdd_cx-supply` (cx.lvl, DT init RETENTION)
        // and the glue node's `qcom,msm-bus,vectors-KBps`. Fastboot's votes
        // die with its exit, so both are re-asserted here for the handoff as
        // well — this is the missing vote behind the ~5-8 s post-attach
        // collapse of the USB clock branch. Unlike the clock retune below,
        // these requests do not disturb a live Fastboot clock domain: they
        // only raise resources the DT already requires for this controller.
        let _ = super::platform::bramble::apply_usb_cx_vote(performance.vote);
        let _ = super::platform::bramble::apply_usb_bus_vote(performance.vote);
        // Select the RCG source before enabling its branch clocks and before
        // publishing the corresponding interconnect vote.  Handoff mode
        // intentionally skips this write because Fastboot owns a live clock
        // domain that must not be retuned underneath the controller.
        if reset_platform {
            if !super::platform::bramble::apply_usb_power(true, super_speed) {
                log_puts("usb: RPMh USB PHY regulator contract unavailable\n");
                return false;
            }
            if !super::platform::bramble::enable_usb30_gdsc() {
                // Some Pixel bootloaders keep the GDSC under secure/RPMh
                // ownership. Treat this as a non-fatal ownership warning.
                log_puts("usb: USB3 GDSC PWR_ON not observable; preserving vote\n");
            }
            if !super::platform::bramble::apply_usb_performance(performance.vote) {
                // A cold platform start may not have an idle Apps-RSC TCS or
                // may reject a GCC update. Preserve the firmware vote/rate
                // rather than issuing a partial secure-owned transaction.
                log_puts(
                    "usb: nominal clock/interconnect transition unavailable; preserving firmware state\n",
                );
            }
        }
        let snpsid = read(GSNPSID);
        log_hex("usb: DWC3 GSNPSID=", snpsid as u64);

        // The Linux lito-usb device tree supplies these clocks and resets to
        // the Qualcomm glue.  A RAM-booted Fullerene image has no clock
        // framework yet, so perform the small branch/reset part directly.
        let mut qmp_ready = if reset_platform {
            let _ = super::platform::bramble::enable_usb_clock_branches();
            // Android's PHY probe enables the RPMh-backed `ref_clk_src`
            // before releasing the PHY/controller reset sequence. This fixed
            // 19.2 MHz clock is separate from the GCC mock-UTMI branch.
            let _ = super::platform::bramble::enable_usb_hs_phy_ref_clock();
            let _ = super::platform::bramble::reset_usb_blocks(super_speed);

            init_hsphy();
            if super_speed { init_qmp_phy() } else { false }
        } else {
            false
        };
        QMP_PHY_READY = qmp_ready;
        // Match the QCOM DWC3 glue's peripheral-mode VBUS override.  The
        // bootloader's fastboot role is not a complete kernel-side OTG
        // session, so relying on the core alone leaves the device halted.
        // The Qualcomm glue asserts the SS-side lane power-present vote even
        // for a USB2-only session; it is the shared Type-C VBUS override path,
        // not a claim that SuperSpeed training completed.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24); // LANE0_PWR_PRESENT
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        // The legacy Qualcomm DWC3 glue enables the master clocks for the
        // controller RAMs here. Without these votes, DWC3 clock gating can
        // shut the RAM interface off even though the core and PHY clocks are
        // running, leaving the event ring and endpoint commands invisible.
        qscratch_set(QSCRATCH_CGCTL, 0x18);
        enable_power_events();
        // Select peripheral mode before issuing the device soft reset. The
        // DCTL.CSFTRST handshake is only defined while the core is in device
        // capability mode; fastboot may have left the port in host/OTG mode.
        let mut gctl = read(GCTL);
        gctl &= !GCTL_PRTCAPDIR_MASK;
        gctl |= GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG;
        write(GCTL, gctl);
        // Capture the previous owner's RAM clock select BEFORE any reset:
        // CSFTRST and the host's bus USB reset both clear GCTL.RAMCLKSEL,
        // and with the wrong select the internal endpoint RAM misroutes
        // writes, which is exactly the "No resource" STARTTRANSFER failure.
        RAMCLK_CAPTURE = gctl_ramclksel(read(GCTL));
        RAMCLK_CAPTURE_VALID = true;
        trace_event(
            TRACE_DWC3_REVISION_QUIRK,
            0x5243_4150,
            RAMCLK_CAPTURE,
            0,
            0,
            0,
        );
        // E0: session bits + GCTL device mode + RAMCLK captured, before the
        // bare-pullup inner (the host attach point is at or before this).
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if let Some(want) = option_env!("FULLERENE_USB_SIGNAL_RAMCLK_GATE") {
            // One-bit readout of the previous owner's GCTL.RAMCLKSEL value.
            if let Ok(value) = want.parse::<u32>() {
                if RAMCLK_CAPTURE != value {
                    trace_marker(TRACE_PROBE_WATCHDOG, 0x5243_4700 | (RAMCLK_CAPTURE & 0xff));
                    log_puts("usb: ramclk gate mismatch; suppressing pull-up\n");
                    return false;
                }
            }
        }

        // B1: GCTL device-mode + RAMCLK capture done, before the bare inner.
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();

        // Use the same pre-reset ownership boundary as the proven Bramble
        // gadget probe.  The helper wakes UTMI, selects the USB2 clock path,
        // clears stale endpoint advertising, and stops the old Fastboot
        // session before CSFTRST.  Repeating those writes inline here had
        // drifted from the working handoff sequence and could leave the
        // controller reset while its PHY/session state was still suspended.
        if !reset_platform && !super_speed {
            let _ = init_usb2_bare_pullup_handoff_inner(false);
        }

        // B2: bare inner done (UTMI awake, GCTL device mode, old Fastboot
        // session stopped and the core halted) - just before core_soft_reset.
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();

        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if cfg!(fullerene_aarch64_usb_gadget_handoff_direct) && stop_after_gadget_handoff_stage(1) {
            return true;
        }

        if !reset_core && !stop_running_device() {
            return false;
        }

        // Linux initializes and resets the external USB2 PHY before issuing
        // DWC3 CSFTRST.  Fastboot normally leaves the PHY live, so retain
        // the existing post-reset initialization as the default and expose
        // the Linux ordering as a bounded handoff A/B.
        #[cfg(fullerene_aarch64_usb_hsphy_before_reset)]
        if reset_core && !super_speed && !reset_platform {
            let _ = super::platform::bramble::pulse_usb2_phy_reset();
            init_hsphy();
        }

        // Android's dwc3_core_soft_reset() initializes the external PHY before
        // issuing DCTL.CSFTRST.  Keep the SuperSpeed handoff at the same
        // ownership boundary: a QMP init after CSFTRST leaves DWC3 and the
        // combo PHY observing different reset epochs, and on Bramble that can
        // turn into either a watchdog or a host-visible USB2-only fallback.
        // The no-core variant has already stopped the old controller above;
        // the normal handoff needs the same stop before moving QMP first.
        if super_speed && !reset_platform {
            if reset_core && !stop_running_device() {
                return false;
            }
            // Match dwc3_phy_setup(): the official core writes the USB3 PIPE
            // suspend bit before dwc3_core_soft_reset() invokes
            // usb_phy_init(usb3), which is the QMP init boundary. Fastboot's
            // retained value is not a valid substitute for that source-
            // ordered write.
            let mut usb3 = read(GUSB3PIPECTL0);
            usb3 |= GUSB3PIPECTL_SUSPHY;
            write(GUSB3PIPECTL0, usb3);
            let _ = read(GUSB3PIPECTL0);
            // Android resets the combo PHY immediately before QMP init.
            // Keep the DWC3 core untouched for --no-core-reset, but restore
            // this separate global/USB3-PHY reset boundary first.
            if !super::platform::bramble::reset_qmp_phy_blocks() {
                log_puts("usb: Fastboot QMP PHY reset failed\n");
                return false;
            }
            // Match msm_ssphy_qmp_init()'s PHY-local ownership boundary:
            // reassert its core LDO, the shared 19.2 MHz reference, and only
            // the QMP AUX/COM_AUX/PIPE branches.  Do not retune the live
            // DWC3 core or mock-UTMI RCGs here; --no-core-reset must remain a
            // PHY/clock differential, not a second controller reset path.
            if !super::platform::bramble::refresh_usb_qmp_power() {
                log_puts("usb: Fastboot QMP regulator refresh failed\n");
            }
            if !super::platform::bramble::enable_usb_hs_phy_ref_clock() {
                log_puts("usb: Fastboot QMP ref clock enable failed\n");
            }
            if !super::platform::bramble::enable_usb_qmp_clock_branches() {
                log_puts("usb: Fastboot QMP clock branch enable failed\n");
                return false;
            }
            qmp_ready = init_qmp_phy();
            QMP_PHY_READY = qmp_ready;
            #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reassert_qmp_power)]
            if qmp_ready {
                let qmp_power = phy::qmp_reassert_power();
                log_hex("usb: SS QMP power reassert mask=", qmp_power as u64);
            }
            #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reassert_core_clocks)]
            if qmp_ready {
                // Fastboot's SS session can leave the DWC3 core domain
                // collapsed while its QMP branches remain usable. Reassert
                // only the source-derived CX/bus votes, USB30 GDSC, and the
                // five controller branches here. No reset and no QMP branch
                // rewrite are part of this A/B, so a changed result points to
                // controller-domain ownership rather than PHY initialization.
                let _ = reassert_ss_controller_domain();
            }
            #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reassert_qmp_clocks)]
            if qmp_ready {
                // The QMP branches depend on the CX/GDSC/controller vote
                // above on the no-core path. Reassert them only after that
                // domain is live, matching the Android clock ownership
                // order rather than attempting the branch write too early.
                let ok = super::platform::bramble::enable_usb_qmp_clock_branches();
                log_hex("usb: SS QMP clock reassert=", u64::from(ok));
            }
            let qmp_probe_phase = qmp_phase_probe_reached();
            if qmp_probe_phase != 0 {
                // The phase probe intentionally stops before the next QMP
                // access and falls back to the known USB2 pull-up. Host
                // attach is therefore a same-boot reached/not-reached bit;
                // it does not claim that the partial QMP state is usable.
                trace_marker(
                    TRACE_PROBE_WATCHDOG,
                    0x5150_0000 | (qmp_probe_phase & 0xff),
                ); // "QP" + phase
                QMP_PHY_READY = false;
                return init_usb2_bare_pullup_handoff_inner(true);
            }
            if !qmp_ready {
                log_puts("usb: Fastboot QMP SuperSpeed handoff unavailable\n");
                return false;
            }
            #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_android_dbm_reset)]
            {
                // Android's `dwc3_msm_block_reset(false)` does not assert
                // the GCC/DWC3 core reset here; it resets and enables the
                // Qualcomm DBM immediately before peripheral-mode startup.
                // Keep this source-ordered A/B before any SS endpoint command
                // and before Run/Stop so it can be distinguished from the
                // failed post-Run/Stop link-clock experiment.
                if !super::platform::bramble::android_dbm_reset_and_enable() {
                    log_puts("usb: Android DBM reset/enable failed\n");
                    return false;
                }
            }
            // `--no-core-reset` skips the normal DWC3 core/PHY soft-reset
            // sequence, including its USB3 PIPE PHYSOFTRST release. The
            // external QMP reset above does not clear this controller-side
            // bit, so expose the source-derived release as an isolated A/B.
            if !reset_core && cfg!(fullerene_aarch64_usb_ss_phy_reset_release) {
                release_usb3_phy_reset();
            }
            // Stage 13 is the QMP-complete boundary. It intentionally
            // publishes only the minimum SS Run/Stop state so host attach can
            // distinguish QMP/link bring-up from the later SMMU, endpoint,
            // event-ring, and EP0 setup sequence.
            #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
            if (cfg!(fullerene_aarch64_usb_gadget_handoff_direct) || super_speed)
                && stop_after_gadget_handoff_stage(13)
            {
                return true;
            }
        }

        if reset_core {
            // B3: about to assert CSFTRST (device_soft_reset inside
            // core_soft_reset). If the log stops after B3, the failure is
            // inside the soft-reset handshake or the core/PHY reset section.
            #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
            init_beacon();
            // Core state at the moment the soft reset is about to start:
            // a CSFTRST issued while the core is suspended/halted vs reset
            // vs running has different completion behavior, and the gate
            // readout names which state the handoff actually inherited.
            INIT_PRE_RESET_DSTS = read(DSTS);
            let reset_ok = if reset_platform {
                core_soft_reset(qmp_ready)
            } else if !super_speed {
                // Linux's reconnect path uses dwc3_core_soft_reset() before
                // rebuilding the event ring and EP0, even when the
                // Qualcomm PHY/clock ownership is retained by firmware.
                // For the USB2 direct handoff this resets only the DWC3
                // device core and USB2 PHY-facing state; the external QUSB2
                // rail, Type-C session, and USB3 PHY remain untouched and
                // are re-applied below.
                core_soft_reset(false)
            } else {
                device_soft_reset()
            };
            if !reset_ok {
                log_puts("usb: DWC3 reset failed\n");
                return false;
            }
        }

        // An SS-only Fastboot session may leave the mock UTMI branch clock
        // gated even though the USB2 PHY can still answer the host's chirp.
        // Bring up that branch after the direct handoff reset, matching the
        // Qualcomm resume order before issuing any DWC3 endpoint command.
        if !super_speed && !reset_platform && !super::platform::bramble::enable_usb2_utmi_clock() {
            log_puts("usb: GCC mock UTMI clock enable failed\n");
            trace_event(TRACE_GCC_UTMI_CLOCK, 0, 0, 0, 0, read(DSTS));
        }

        // B4: core_soft_reset (CSFTRST + GCTL core reset + USB2 PHY
        // soft reset + release delays) fully returned.
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();

        configure_dwc3_global_control();
        // CSFTRST can restore GCTL.PRTCAPDIR to its reset value on DWC_usb31.
        // Keep the controller in device mode before programming EP0, while
        // preserving the platform-specific global-control bits.
        let mut gctl = read(GCTL);
        gctl &= !(GCTL_PRTCAPDIR_MASK | GCTL_SCALEDOWN_MASK | GCTL_DISSCRAMBLE);
        gctl |= GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG;
        write(GCTL, gctl);
        let _ = read(GCTL);
        // DWC_usb31 skips the generic global-control helper's RAMCLKSEL
        // restore because its revision is encoded in VER_NUMBER rather than
        // GSNPSID. The direct handoff captured the working Fastboot value
        // before CSFTRST; restore it before any endpoint-context command.
        reapply_ramclksel();
        // The generic global-control helper intentionally returns early for
        // DWC_usb31, which is the IP used by Bramble. Reapply the USB2 UTMI
        // interface contract and the msm-4.19 usb31 gadget workarounds here;
        // otherwise CSFTRST leaves the DWC3-side timing at its reset/Fastboot
        // value and the host can see the pull-up without EP0 transactions.
        configure_usb2_phy_interface();
        apply_usb31_gadget_reference_deltas();
        // The USB2 handoff already applies Android msm's second
        // dwc3_set_mode(DEVICE) GCTL write. The SuperSpeed path must carry
        // the same DWC_usb31 mode tail: U2RSTECN, PWRDNSCALE=2, and
        // U2EXIT_LFPS are controller-mode state, not USB2 packet framing.
        // Apply it before the stage-14 SS boundary and before publishing any
        // endpoint resources.
        configure_dwc3_device_mode();

        // Stage 14 is the post-QMP DWC3 global-control boundary. It keeps the
        // core reset differential unchanged, but includes the device-mode,
        // RAM-clock, USB2 timing, and USB31 reference setup that the normal
        // path performs after QMP becomes ready. The stage helper then emits
        // only DCFG=SS and Run/Stop, before SMMU or endpoint/DMA ownership.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(14) {
            return true;
        }

        // B5: post-reset global control programmed.
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();

        INIT_STAGE = 2;
        // The device reset above is the ownership boundary for the previous
        // Fastboot transfer epoch. The direct probe normally enters before
        // usb_probe_entry's fallback allocator setup, so initialize the
        // linker-owned event/TRB objects here, after reset and before any
        // address is published to DWC3.
        if reset_core {
            clear_dma_memory();
        }

        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if cfg!(fullerene_aarch64_usb_gadget_handoff_direct) && stop_after_gadget_handoff_stage(2) {
            return true;
        }

        // Stage 15 is the first post-stage-14 tail boundary.  In the usual
        // `--no-core-reset` run the clear is a no-op, which is intentional:
        // this stop still separates the stage-14 register path from the
        // first SMMU/ownership operation below.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(15) {
            return true;
        }

        // Fastboot leaves the USB2 PHY powered, but the DWC3 handoff can
        // clear the PHY's session-valid state while stopping the old gadget.
        // Reapply the non-destructive Femto PHY programming on the USB2
        // handoff path; this does not assert the GCC PHY reset or touch the
        // Type-C power domain.
        if !super_speed && !reset_platform {
            init_hsphy();
        }

        // Core reset restores the QSCRATCH-facing state on some DWC3
        // revisions, so re-apply the Qualcomm glue votes after reset.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24); // LANE0_PWR_PRESENT
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        qscratch_set(QSCRATCH_CGCTL, 0x18);
        // SM7250's DWC3 revision is older than 2.50a. The Qualcomm glue
        // advertises the XHCI 1.0 register layout through this QSCRATCH bit
        // during its reset callback.
        qscratch_set(QSCRATCH_GENERAL_CFG, QSCRATCH_GENERAL_CFG_XHCI_REV);

        // C1: post-reset QSCRATCH session re-asserted (the host attach point
        // when the QSCRATCH session bits own the pull-up).
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();

        // USB2-only starts need the same post-reset UTMI clock selection as
        // the Qualcomm glue. The DWC3 reset above invalidates the controller's
        // previous PIPE/UTMI selection, so this is required for handoff too;
        // it is a controller-side QSCRATCH mux change, not a PHY power reset.
        if !super_speed {
            select_utmi_pipe_clock();
        }

        // Match dwc3_msm_update_ref_clk() from the Qualcomm glue. This is a
        // controller post-reset setting, so it must also run after a
        // Fastboot handoff reset; it does not retune the GCC source clock.
        update_dwc3_ref_clock();

        // Linux/Android install the Apps-SMMU context before the DWC3 gadget
        // receives a request. A `fastboot boot` image has no IOMMU framework
        // to inherit that ownership, so the handoff must do the equivalent
        // after the old DWC3 session has been stopped/reset and before any
        // Fullerene event/TRB address is published. This is deliberately
        // performed for both cold and Fastboot paths; preserving a live
        // firmware mapping while using a different DMA pool is not a valid
        // non-destructive handoff.
        let smmu_ready = if cfg!(all(
            fullerene_aarch64_usb_gadget_handoff_probe,
            fullerene_aarch64_usb_gadget_handoff_no_smmu
        )) {
            // Keep the direct probe's no-SMMU differential meaningful: it
            // must not partially rewrite the Apps-SMMU before testing the
            // firmware-owned physical=IOVA bypass.
            trace_event(TRACE_SMMU_PRESERVED, 0, 0, 0, 0, 0);
            true
        } else {
            configure_dwc3_smmu()
        };
        trace_event(
            TRACE_SMMU_HANDOFF,
            smmu_ready as u32,
            reset_platform as u32,
            super::platform::bramble::usb_resources().dma_pool.stream_id,
            super::platform::bramble::usb_resources().dma_pool.iova_base as u32,
            super::platform::bramble::usb_resources().dma_pool.size as u32,
        );
        if smmu_ready {
            log_puts("usb: DWC3 SMMU DMA-pool map ready\n");
        } else {
            // Proceeding with an unverified IOVA map would turn the first
            // SETUP TRB into an opaque DMA fault, so let the caller choose its
            // explicit recovery/fallback path.
            log_puts(if reset_platform {
                "usb: DWC3 SMMU DMA-pool map unavailable\n"
            } else {
                "usb: Fastboot SMMU handoff map unavailable\n"
            });
            return false;
        }

        // Stage 16 proves that the selected physical/no-SMMU or Apps-SMMU
        // branch returned before any event-ring address is handed to DWC3.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(16) {
            return true;
        }

        INIT_STAGE = 3;
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if cfg!(fullerene_aarch64_usb_gadget_handoff_direct) && stop_after_gadget_handoff_stage(3) {
            return true;
        }

        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        write(GUSB2PHYCFG0, usb2);
        // Match dwc3_dis_sleep_mode(): the host-side L1 threshold helper is
        // independent of the USB2 PHY sleep bit and can survive a Fastboot
        // handoff with a stale value.
        let guctl1 = read(GUCTL1);
        write(GUCTL1, guctl1 & !GUCTL1_L1_SUSP_THRLD_EN_FOR_HOST);
        let mut usb3 = read(GUSB3PIPECTL0);
        if qmp_ready {
            usb3 &= !GUSB3PIPECTL_SUSPHY;
        } else {
            // Keep the USB2 gadget usable if the board-specific SuperSpeed
            // calibration does not reach PHY ready.
            usb3 |= GUSB3PIPECTL_SUSPHY;
        }
        write(GUSB3PIPECTL0, usb3);

        let event_address = ep0_event_address();
        // The event ring lives in the normal-cacheable early heap mapping.
        // Evict any CPU-side zero-fill before handing the buffer to DWC3;
        // otherwise a later cache writeback could overwrite an event that the
        // controller has already posted.
        cache_clean(ep0_event_dma_base(), ep0_event_size());
        write(GEVNTADRLO0, event_address as u32);
        write(GEVNTADRHI0, (event_address >> 32) as u32);
        write(GEVNTSIZ0, ep0_event_size() as u32);
        acknowledge_ep0_event_count();
        // C2: event ring published and the Fastboot event count acknowledged.
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();
        trace_event(
            TRACE_EVENT_RING_READY,
            event_address as u32,
            (event_address >> 32) as u32,
            EVENT_BUFFER_SIZE as u32,
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

        // Stage 17 is after the event-ring publication, optional Qualcomm GSI
        // setup, and the in-memory gadget-state reset, but before DCFG and
        // endpoint-context commands.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(17) {
            return true;
        }

        // The bootloader may leave DCFG in the speed/address state of its
        // Fastboot session. Reset both fields explicitly before enabling the
        // pull-up; Linux's gadget path selects the maximum PHY-backed speed
        // at the same point in its start sequence.
        let mut dcfg = read(DCFG) & !(DCFG_SPEED_MASK | DCFG_DEVADDR_MASK);
        // DCFG.SPEED must match a PHY the transfer engine can actually use
        // at Start Transfer time. With DCFG=SuperSpeed on a USB2-only handoff
        // (QMP absent), the SS link can never train and every EP0
        // STARTTRANSFER completes with "No resource" — the proven-working
        // fallback path programs DCFG_HIGHSPEED here and its EP0 pipeline
        // runs end to end. Linux's SuperSpeed-default convention only holds
        // when a SuperSpeed PHY is present (qmp_ready).
        dcfg |= if qmp_ready {
            DCFG_SUPERSPEED
        } else {
            DCFG_HIGHSPEED
        };
        write(DCFG, dcfg);
        configure_gadget_start_defaults();

        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if cfg!(fullerene_aarch64_usb_gadget_handoff_direct) && stop_after_gadget_handoff_stage(4) {
            return true;
        }

        // Capture the core state BEFORE the first endpoint command: the
        // post-command DSTS (below) only reflects the state after the
        // command retired or its 2M-read timeout expired.
        INIT_DEPSTART_PRE_DSTS = read(DSTS);
        let depstart_ok = send_ep_command(0, DEPCMD_DEPSTARTCFG, 0, 0, 0);
        INIT_STAGE = 4;
        INIT_DEPSTART_RAW = read(dep_reg(0, 0x0c));
        INIT_DEPSTART_DSTS = read(DSTS);
        if !depstart_ok {
            log_puts("usb: DEPSTARTCFG failed\n");
            return false;
        }
        // Android's msm DWC3 glue preallocates every hardware endpoint's
        // transfer resource immediately after DEPSTARTCFG. The earlier
        // fallback-path experiment did not cover this direct sequence, so
        // keep the direct-path differential explicit as well.
        if cfg!(fullerene_aarch64_usb_gadget_handoff_android_resource_order)
            && !cfg!(fullerene_aarch64_usb_gadget_handoff_no_transfer_resource)
        {
            for endpoint in 0..32usize {
                if !set_transfer_resource(endpoint) {
                    log_puts("usb: Android direct resource preallocation failed\n");
                    return false;
                }
            }
        }
        // Linux starts EP0 at the SuperSpeed packet size and changes it at
        // Connect Done, even when the eventual link falls back to High
        // Speed. Keep the direct USB2 handoff's smaller configuration as the
        // default for the known-good Bramble path, but expose the exact
        // Linux initial state as a bounded hardware A/B.
        let ep0_packet_size = if cfg!(fullerene_aarch64_usb_ep0_initial_512) || qmp_ready {
            INITIAL_EP0_MAX_PACKET_SIZE
        } else {
            64
        };
        let epcfg0 = configure_endpoint(0, ep0_packet_size, false);
        INIT_STAGE = 5;
        INIT_EPCFG0_OK = epcfg0;
        INIT_EPCFG0_RAW = read(dep_reg(0, 0x0c));
        INIT_EPCFG0_DSTS = read(DSTS);
        let epcfg1 = if epcfg0 {
            configure_endpoint(1, ep0_packet_size, false)
        } else {
            false
        };
        INIT_EPCFG1_OK = epcfg1;
        if epcfg0 {
            INIT_EPCFG1_RAW = read(dep_reg(1, 0x0c));
            INIT_EPCFG1_DSTS = read(DSTS);
        }
        if epcfg0 {
            INIT_STAGE = 6;
        }
        if !epcfg0 || !epcfg1 {
            log_puts("usb: EP0 configuration failed\n");
            return false;
        }

        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if cfg!(fullerene_aarch64_usb_gadget_handoff_direct) && stop_after_gadget_handoff_stage(5) {
            return true;
        }
        ENDPOINTS_READY = true;
        let _ = udc_mut().configure_endpoint(0, ep0_packet_size as u16, false);
        let _ = udc_mut().configure_endpoint(1, ep0_packet_size as u16, false);
        write(DALEPENA, 0b11);
        write(
            DEVTEN,
            DEVTEN_DISCONNECT
                | DEVTEN_USB_RESET
                | DEVTEN_CONNECT_DONE
                | DEVTEN_LINK_STATUS_CHANGE
                | DEVTEN_WAKEUP
                | DEVTEN_HIBERNATION_REQUEST
                | DEVTEN_SUSPEND
                | DEVTEN_ERRATIC_ERROR
                | DEVTEN_CMD_COMPLETE
                | DEVTEN_OVERFLOW,
        );
        trace_event(TRACE_SETUP_QUEUED, 0, 0, 0, 8, read(DSTS));

        // Stage 18 stops after the endpoint contexts, DALEPENA, and DEVTEN
        // are published, before the first setup-TRB write.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(18) {
            return true;
        }

        prepare_ep0_setup_trb();

        #[cfg(fullerene_aarch64_usb_gadget_handoff_ep0_stall_flush)]
        {
            // Fastboot's interrupted session can leave a SETUP packet pending
            // in the EP0 FIFO, and this core rejects every endpoint command
            // until the pending packet is flushed - the host's first SETUP
            // token then goes unanswered until its own retry arrives seconds
            // later. Linux's dwc3_ep0_stall_and_restart() clears exactly this
            // state with an EP0 SETSTALL; the core auto-clears the stall when
            // the next SETUP token arrives, so arm the fresh SETUP TRB at the
            // same halted boundary Linux uses.
            let _ = unsafe { send_ep_command(0, DEPCMD_SETSTALL, 0, 0, 0) };
            trace_event(
                TRACE_SETUP_QUEUED,
                0x5354_4C46, // "STLF" stall-flush arm outcome
                EP0_SETUP_ARMED as u32,
                0,
                0,
                read(DSTS),
            );
        }
        // C3: DEPSTARTCFG + both EP0 SETEPCONFIG/SETTRANSFRESOURCE commands
        // done (or failed fast), DALEPENA/DEVTEN set, setup TRB prepared.

        apply_ep0_txfifo_fix();
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();

        // Stage 19 is the setup-TRB/cache boundary.  DMA probing, transfer
        // commands, IRQ routing, and the final Run/Stop sequence are all
        // deliberately downstream of this stop.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(19) {
            return true;
        }

        // Split the direct probe at the exact DMA publication boundary:
        // stage 6 has only written/cleaned the setup TRB, while stage 7 is
        // after the DWC3 STARTTRANSFER command has retired. This makes a
        // cache/SMMU/TRB fault distinguishable from a command-state failure.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if cfg!(fullerene_aarch64_usb_gadget_handoff_direct) && stop_after_gadget_handoff_stage(6) {
            return true;
        }

        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if option_env!("FULLERENE_USB_SIGNAL_DMA_PROBE") == Some("1") {
            // Event-DMA liveness probe. The endpoint is fully configured here
            // (DEPSTARTCFG, SETEPCONFIG, SETTRANSFRESOURCE done, TRB armed):
            // arm a real SETUP transfer on ep0 OUT and then ENDTRANSFER it
            // with CMDIOC — the exact Linux stop-active-transfer pattern, so
            // the core must post the completion event. GEVNTCOUNT > 0 proves
            // the core's event DMA reaches DRAM; gate the pull-up off when it
            // never arrives so the host-visible attach names a working DMA
            // path.
            //
            // Clear any latched Apps-SMMU faults first so the post-probe FSR
            // names only this attempt's DMA attempts.
            let fsr_before = read_volatile(smmu_reg(SMMU_GR0_FSR));
            if fsr_before != 0 && fsr_before != u32::MAX {
                write_volatile(smmu_reg(SMMU_GR0_FSR), fsr_before);
                core::arch::asm!("dsb sy", options(nostack));
            }
            // RAM readback gate: if the linker-reserved .usb_dma window is
            // not backed by real DRAM, every DMA write (event ring, TRB
            // fetch, setup buffer) vanishes and the CPU cannot detect it.
            // Write a pattern, evict it from the cache, and read it back;
            // gate the attach on the pattern surviving.
            if option_env!("FULLERENE_USB_SIGNAL_RAM_GATE") == Some("1") {
                // Verify EVERY object the controller will DMA, not just the
                // event ring: a partially backed region can pass the first
                // page while the TRB/SETUP pages hang the core's fetch.
                let mut ram_ok = true;
                let targets: [(usize, usize); 3] = [
                    (ep0_event_dma_base(), 16),
                    (ep0_trb_ptr(0) as usize, 64),
                    (ep0_response_ptr() as usize, 512),
                ];
                for (address, span) in targets {
                    let pattern = [0xA55A_5AA5u32, 0x1234_5678, 0xDEAD_BEEF, 0x0BAD_C0DE];
                    let words = span / 4;
                    for offset in 0..words {
                        unsafe {
                            write_volatile(
                                (address + offset * 4) as *mut u32,
                                pattern[offset % pattern.len()],
                            );
                        }
                    }
                    cache_clean(address, span);
                    cache_invalidate(address, span);
                    for offset in 0..words {
                        unsafe {
                            if read_volatile((address + offset * 4) as *const u32)
                                != pattern[offset % pattern.len()]
                            {
                                ram_ok = false;
                            }
                        }
                    }
                    for offset in 0..words {
                        unsafe { write_volatile((address + offset * 4) as *mut u32, 0) };
                    }
                    cache_clean(address, span);
                }
                trace_event(
                    TRACE_EVENT_RING_READY,
                    0x5241_4D00 | ram_ok as u32,
                    0,
                    0,
                    0,
                    0,
                );
                if !ram_ok {
                    trace_marker(TRACE_PROBE_WATCHDOG, 0x5241_4D46); // "RAMF"
                    log_puts("usb: .usb_dma readback failed; region is not usable RAM\n");
                    return false;
                }
            }
            let started = start_transfer(0, ep0_trb_ptr(0));
            let resource = if started {
                EP0_RESOURCE_INDEX[0].max(1)
            } else {
                1
            };
            let _ = send_ep_command(
                0,
                DEPCMD_ENDTRANSFER
                    | DEPCMD_CMDIOC
                    | DEPCMD_HIPRI_FORCERM
                    | ((resource as u32) << DEPCMD_PARAM_SHIFT),
                0,
                0,
                0,
            );
            EP0_RESOURCE_INDEX[0] = 0;
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
                // GEVNTCOUNT counts the core-internal event FIFO, not the
                // DMA completion. Read the ring slot the event should have
                // landed in: a zero word means the DMA write never reached
                // DRAM (stalled/blocked), which no amount of register setup
                // can mask.
                let slot = (unsafe { EVENT_OFFSET } % unsafe { ep0_event_size() }) & !0x3;
                let word = unsafe { read_volatile((ep0_event_dma_base() + slot) as *const u32) };
                event_word = word;
            }
            let fsr_after = read_volatile(smmu_reg(SMMU_GR0_FSR));
            trace_event(
                TRACE_EVENT_RING_READY,
                delivered as u32,
                event_word,
                fsr_after,
                0,
                0,
            );
            // Event-data gate: 1 = attach only when the event word actually
            // landed in DRAM, 2 = attach only when the ring slot stayed zero.
            match option_env!("FULLERENE_USB_SIGNAL_EVT_DATA_GATE") {
                Some("1") if event_word == 0 => {
                    trace_marker(TRACE_PROBE_WATCHDOG, 0x4556_4430); // "EVD0"
                    log_puts("usb: event word never landed in DRAM\n");
                    return false;
                }
                Some("2") if event_word != 0 => {
                    trace_marker(TRACE_PROBE_WATCHDOG, 0x4556_4431); // "EVD1"
                    log_puts("usb: event word landed but gate wanted zero\n");
                    return false;
                }
                _ => {}
            }
            // FSR gate (one bit per run): 1 = attach only when the SMMU
            // recorded a fault during the probe, 2 = attach only when it did
            // not. This separates "SMMU kills the DMA" from "the core's DMA
            // engine is dead".
            let fsr_gate = option_env!("FULLERENE_USB_SIGNAL_FSR_GATE");
            if fsr_gate == Some("1") || fsr_gate == Some("2") {
                let faulted = fsr_after != u32::MAX && fsr_after != 0;
                let wanted = fsr_gate == Some("1");
                if faulted != wanted {
                    trace_marker(TRACE_PROBE_WATCHDOG, 0x4653_5200 | (fsr_after & 0xff));
                    log_puts("usb: FSR gate mismatch; suppressing pull-up\n");
                    return false;
                }
            }
            if !delivered {
                trace_marker(TRACE_PROBE_WATCHDOG, 0x444D_4146); // "DMAF"
                log_puts("usb: event DMA probe found no delivered event\n");
                return false;
            }
            // Drain the probe events and re-arm a clean SETUP TRB so the
            // normal flow starts from the same state as a non-probe run.
            poll_ep0_event_ring();
            EVENT_OFFSET = 0;
            prepare_ep0_setup_trb();
        }

        // Linux arms the initial EP0 OUT SETUP transfer before Run/Stop. Keep
        // that as the default, but retain a Bramble-only hardware differential
        // for controllers whose firmware handoff cannot tolerate DMA ownership
        // changing while the device is still halted. In that mode the same
        // prepared TRB is armed immediately after Run/Stop, before the host's
        // first descriptor request can be serviced.
        #[cfg(not(any(
            fullerene_aarch64_usb_gadget_handoff_start_after_connect,
            fullerene_aarch64_usb_gadget_handoff_start_after_reset,
            fullerene_aarch64_usb_gadget_handoff_start_at_connect_done
        )))]
        {
            // On this core a Start Transfer issued before the link reaches
            // ON not only fails with "No resource" but WEDGES the endpoint
            // command engine - the later Run/Stop then never publishes the
            // pull-up at all. Do not issue it here: the Connect Done handler
            // arms the SETUP TRB the moment the link comes up (which is
            // still before the host's first SETUP token), and the poll-loop
            // guard re-arms on any later reset.

            #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
            if cfg!(fullerene_aarch64_usb_gadget_handoff_direct)
                && stop_after_gadget_handoff_stage(7)
            {
                return true;
            }
        }
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_direct) {
            enable_gadget_controller_irq();
        }
        // Linux starts consuming DWC3 events as soon as the initial EP0 OUT
        // SETUP transfer is armed. Do the same once before Run/Stop while the
        // early boot path is still polling rather than handling IRQs.
        poll_ep0_event_ring();

        // Use the same Linux-compatible Run/Stop preparation as the probe
        // path. In particular, do not inherit KEEP_CONNECT or the Fastboot
        // HIRD threshold across the temporary-image handoff.
        configure_gadget_speed(qmp_ready);
        // A USB2-only Bramble handoff must leave the UTMI PHY awake, matching
        // the known-good bare/reuse path. The SuperSpeed path still follows
        // Linux's gadget-start SUSPHY policy.
        if qmp_ready {
            enable_gadget_susphy();
        }
        // C4: speed/SUSPHY configured; the next boundary is Run/Stop.
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if let Some(want) = option_env!("FULLERENE_USB_SIGNAL_RSC_GATE") {
            // One-bit readout of the previous attempt's SETTRANSFRESOURCE
            // raw DEPCMD register (resource index 22:16, status 15:12). A
            // healthy allocation returns 0x10000 (index 1, status 0).
            let ok = u32::from_str_radix(want.trim_start_matches("0x"), 16)
                .map(|value| TRACE_HARVEST_RSC == value)
                .unwrap_or(false);
            trace_event(
                TRACE_SMMU_HANDOFF,
                0x5253_4300,
                TRACE_HARVEST_RSC,
                ok as u32,
                0,
                0,
            );
            if !ok {
                trace_marker(
                    TRACE_PROBE_WATCHDOG,
                    0x5253_4300 | (TRACE_HARVEST_RSC & 0xff),
                );
                log_puts("usb: resource gate mismatch; suppressing pull-up\n");
                return false;
            }
        }

        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if let Some(want) = option_env!("FULLERENE_USB_SIGNAL_CFG_GATE") {
            // One-bit readout of the previous attempt's DEPSTARTCFG raw
            // DEPCMD register (returned XferRscIdx 22:16, status 15:12).
            let ok = u32::from_str_radix(want.trim_start_matches("0x"), 16)
                .map(|value| TRACE_HARVEST_CFG == value)
                .unwrap_or(false);
            trace_event(
                TRACE_SMMU_HANDOFF,
                0x5243_4647,
                TRACE_HARVEST_CFG,
                ok as u32,
                0,
                0,
            );
            if !ok {
                trace_marker(
                    TRACE_PROBE_WATCHDOG,
                    0x5243_4647 | (TRACE_HARVEST_CFG & 0xff),
                );
                log_puts("usb: cfg gate mismatch; suppressing pull-up\n");
                return false;
            }
        }

        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if let Some(want) = option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") {
            // The gate is evaluated by the signal probe AFTER this run's
            // observation window (see run_ep0_signal_probe): evaluating it
            // here would read attempt 1's still-empty trace and park before
            // any data existed. Keep this marker for the retained trace.
            trace_event(
                TRACE_SMMU_HANDOFF,
                0x434D_4741, // "CMGA"
                0,
                0,
                0,
                0,
            );
            let _ = want;
        }
        #[cfg(not(fullerene_aarch64_usb_ep0_signal_probe))]
        if let Some(want) = option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") {
            // One-bit readouts of the previous attempt's command outcomes and
            // SETUP reception. The retained-trace harvest carries the raw
            // DEPCMD register values; the host-visible attach names them:
            //   "timeout"   -> OLDEST STARTTRANSFER timed out (CMDACT stuck)
            //   "done"      -> OLDEST STARTTRANSFER completed (any status)
            //   "last-timeout" / "last-done" -> NEWEST STARTTRANSFER outcome
            //   "setup"     -> at least one SETUP packet reached DRAM
            //   "none"      -> no STARTTRANSFER record was found
            //   hex value   -> the OLDEST raw DEPCMD register equals exactly
            //                  this value
            let ok = match want {
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
                "connect" => TRACE_HARVEST_CONNECT > 0,
                // Watchdog-state readouts: the host-visible attach names
                // whether the apps watchdog was ARMED at probe entry.
                // Attach only when the guard's arm preceded the host's first
                // SETUP token: the arm won the race.
                "arm-first" => {
                    TRACE_HARVEST_ARM_SEQ != 0xFFFF_FFFF
                        && TRACE_HARVEST_SETUP_SEQ != 0xFFFF_FFFF
                        && TRACE_HARVEST_ARM_SEQ < TRACE_HARVEST_SETUP_SEQ
                }
                // Attach only when the first SETUP arrived while no TRB was
                // armed: the arm lost the race (the -110 root cause).
                "setup-first" => {
                    TRACE_HARVEST_SETUP_SEQ != 0xFFFF_FFFF
                        && (TRACE_HARVEST_ARM_SEQ == 0xFFFF_FFFF
                            || TRACE_HARVEST_ARM_SEQ > TRACE_HARVEST_SETUP_SEQ)
                }
                "wdt-armed" => WDT_KPSS_EN_AT_ENTRY & 1 != 0,
                "wdt-off" => WDT_KPSS_EN_AT_ENTRY != 0xFFFF_FFFF && WDT_KPSS_EN_AT_ENTRY & 1 == 0,
                "scm-answ" => (SWDD_AVAIL & 0xFFFF_FFFF) != 0xFFFF_FFFF,
                "scm-avail" => (SWDD_AVAIL & 0xFFFF_FFFF) == 1,
                "scm-noimpl" => (SWDD_AVAIL & 0xFFFF_FFFF) == 0,
                "scm-dead" => (SWDD_AVAIL & 0xFFFF_FFFF) == 0xFFFF_FFFF,
                "std-ok" => SWDD_STD != 0xFFFF_FFFF && (SWDD_STD & 0xFFFF_FFFF) > 0xFFFF,
                "std-dead" => SWDD_STD == 0xFFFF_FFFF,
                "mdcr-trap" => MDCR_EL2_AT_ENTRY & (1 << 14) != 0,
                "mdcr-clean" => MDCR_EL2_AT_ENTRY != u64::MAX && MDCR_EL2_AT_ENTRY & (1 << 14) == 0,
                "el1" => CURRENT_EL_AT_ENTRY & 0xF == 0b0100,
                "el2" => CURRENT_EL_AT_ENTRY & 0xF == 0b1000,
                "addr" => TRACE_HARVEST_ADDR > 0,
                "readall" => TRACE_HARVEST_ADDR2 > 0,
                "second-setup" => TRACE_HARVEST_SETUP >= 2,
                // Attach only when the first SETUP arrived within 2 seconds
                // of Connect Done, i.e. inside the host's enumeration window.
                "setup-fast" => TRACE_HARVEST_SETUP > 0 && TRACE_HARVEST_SETUP_DELAY <= 2,
                // Attach only when a SETUP arrived but LATE (> 2 seconds
                // after Connect Done): the pipeline ran after the host gave
                // up, which is a pure timing failure.
                "setup-slow" => TRACE_HARVEST_SETUP > 0 && TRACE_HARVEST_SETUP_DELAY > 2,
                // The timeout flag is bit 31; bit 16 alone is a healthy
                // XferRscIdx=1 completion on physical EP1.
                "ep1-done" => {
                    TRACE_HARVEST_EP1 != 0xFFFF_FFFF
                        && TRACE_HARVEST_EP1 & 0x8000_0000 == 0
                        && TRACE_HARVEST_EP1 & 0xf000 == 0
                }
                "ep1-1000" => TRACE_HARVEST_EP1 == 0x1000,
                "none" => TRACE_HARVEST == 0xFFFF_FFFF,
                other => u32::from_str_radix(other.trim_start_matches("0x"), 16)
                    .map(|value| TRACE_HARVEST == value)
                    .unwrap_or(false),
            };
            trace_event(
                TRACE_SMMU_HANDOFF,
                0x434D_4400,
                TRACE_HARVEST,
                TRACE_HARVEST_LAST,
                TRACE_HARVEST_SETUP | (TRACE_HARVEST_DESC << 16),
                ok as u32,
            );
            if !ok {
                trace_marker(TRACE_PROBE_WATCHDOG, 0x434D_4400 | (TRACE_HARVEST & 0xff));
                log_puts("usb: command gate mismatch; suppressing pull-up\n");
                park_after_gate_failure();
            }
        }

        #[cfg(fullerene_aarch64_usb_ep0_smmu_gate)]
        {
            // One-bit SMMU readout: publish the pull-up only when the
            // stream's S2CR type matches the requested value, so the
            // host-visible attach itself names the Apps-SMMU stream state.
            // Parse the full value: the ladder codes 3 and 251..=254 are
            // equally valid gate targets as the raw S2CR types 0..=2.
            let want = option_env!("FULLERENE_USB_SMMU_GATE_TYPE")
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(99);
            let actual = smmu_stream_s2cr_type();
            trace_event(TRACE_SMMU_HANDOFF, actual, want, 0, 0, read(DSTS));
            if actual != want {
                trace_marker(TRACE_PROBE_WATCHDOG, 0x534d_4d55 | (actual & 0xff));
                log_puts("usb: SMMU gate mismatch; suppressing pull-up\n");
                return false;
            }
        }

        #[cfg(fullerene_aarch64_usb_ep0_smmu_install)]
        {
            // The stream is unmatched (ladder 252): with an active SMMU every
            // DWC3 DMA faults, which is exactly the dead-event-ring / dead-EP0
            // symptom. Claim a free SMR and point it at BYPASS so DMA passes
            // untranslated. The gate is STRICT: only a verified install on an
            // active-and-unmatched stream publishes the pull-up, so the
            // host-visible attach names exactly this state.
            let before = smmu_stream_s2cr_type();
            let installed = before == 252 && smmu_install_stream_bypass();
            trace_event(
                TRACE_SMMU_HANDOFF,
                0x494E_5354,
                installed as u32,
                before,
                0,
                0,
            );
            if !installed {
                trace_marker(TRACE_PROBE_WATCHDOG, 0x5349_4E46); // "SINF"
                log_puts("usb: SMMU stream install rejected; suppressing pull-up\n");
                return false;
            }
        }

        #[cfg(fullerene_aarch64_usb_ep0_dma_adopt)]
        if !dma_mapping_adopted() {
            // The stream was not in a rewritable TRANSLATE context or the
            // page-table walk could not adopt a mapped page. Without a known
            // DMA window the EP0 path cannot work, so leave the pull-up
            // down: the host-visible ABSENCE of the attach is the one-bit
            // readout naming this branch.
            trace_marker(TRACE_PROBE_WATCHDOG, 0x534e_4f44); // "SNOD"
            log_puts("usb: no adopted SMMU window; suppressing pull-up\n");
            return false;
        }
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        {
            // Timing channel: delay ONLY the first attempt's connect by a
            // fixed number of seconds. The host's attach timestamp relative
            // to the Fastboot-device disconnect in the same journal then
            // shows whether Run/Stop owns the physical pull-up or an earlier
            // init stage (e.g. init_hsphy's VBUSVLDEXT0) asserts it.
            let first_attempt = !SIGNAL_CONNECT_DELAYED;
            SIGNAL_CONNECT_DELAYED = true;
            if first_attempt {
                if let Some(secs) = option_env!("FULLERENE_USB_CONNECT_DELAY_SECS")
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|value| *value > 0)
                {
                    trace_marker(TRACE_PROBE_WATCHDOG, 0x4344_4C59); // "CDLY"
                    super::timer::delay_ms(secs.saturating_mul(1000));
                }
            }
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_event_ring_at_runstop)]
        republish_ep0_event_ring_at_runstop();
        #[cfg(fullerene_aarch64_usb_gadget_handoff_gadget_restart_at_runstop)]
        if !restart_gadget_at_runstop() {
            // Android's dwc3_gadget_run_stop() ignores the void restart
            // helper's internal command result and still asserts Run/Stop.
            // Keep that behavior for this diagnostic so a failed EP0 restart
            // is distinguishable from a failure to publish the pull-up.
            log_puts("usb: Android gadget restart at Run/Stop incomplete\n");
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_susphy)]
        enable_usb2_gadget_susphy();

        // Stage 20 is the last pre-Run/Stop boundary.  It includes all
        // setup/ownership work above and leaves only the actual production
        // Run/Stop transition plus post-connect arm logic unexecuted.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && gadget_handoff_stop_selected(20) {
            #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
            if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE")
                .filter(|value| value.starts_with("ss-"))
                .is_some()
            {
                // Capture before the selected stage helper performs its
                // diagnostic recovery. This is the actual pre-production
                // boundary; capturing after the helper would describe its
                // extra Run/Stop instead.
                SS_RUNSTOP_PRE_DCTL = read(DCTL);
                capture_ss_state_snapshot();
            }
            #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
            if let Some(selector) = option_env!("FULLERENE_USB_SIGNAL_CMD_GATE")
                .filter(|value| {
                    value.starts_with("ss-domain-")
                        || value.starts_with("ss-gctl-")
                        || value.starts_with("ss-qmp-")
                        || value.starts_with("ss-qscratch-")
                        || *value == "ss-ltssm"
                        || value.starts_with("ss-ltssm-bit")
                })
            {
                // DCTL stop/run is not a reliable physical readout at this
                // boundary. Encode one domain/GCTL/QMP bit in the time at which the
                // known HS fallback is published: 0 = no extra delay, 1 =
                // 4 seconds, and 2 = missing snapshot sentinel. The 4 s
                // separation is wider than the observed host attach jitter
                // while remaining below the handset's watchdog window.
                let code = utmi_readout_code(selector);
                let delay_ms = if selector == "ss-ltssm" {
                    // Raw LTSSM values use a compact 250-ms bucket. The
                    // missing-snapshot sentinel is 16 and remains bounded
                    // below the normal watchdog recovery window.
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
            if stop_after_gadget_handoff_stage(20) {
                return true;
            }
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reassert_device_mode)]
        if super_speed {
            // The post-Run/Stop `ss-gctl=0` readout indicates that this
            // DWC_usb31 instance is not retaining PRTCAPDIR=DEVICE. Reapply
            // only the Android msm device-mode tail immediately before the
            // production transition; leave PHY, endpoint, and packet state
            // untouched for this controller-only A/B.
            configure_dwc3_device_mode();
        }
        if super_speed && cfg!(fullerene_aarch64_usb_ep0_signal_probe) {
            SS_RUNSTOP_PRE_DCTL = read(DCTL);
        }
        if !run_stop_device(true) {
            log_hex("usb: DWC3 remained halted, DSTS=", read(DSTS) as u64);
            return false;
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reassert_link_clocks_after_runstop)]
        if super_speed {
            // Qualcomm's msm glue treats the link-clock reset as a separate
            // boundary from DWC3 CSFTRST. Replay that source-ordered
            // iface/core/sleep/utmi stop -> GCC core-reset -> release sequence
            // only after the production Run/Stop write, so this A/B answers
            // whether the post-transition domain loss is an omitted glue
            // ownership boundary. The stage-21 snapshot follows immediately;
            // no endpoint or packet operation is added to the test.
            trace_marker(TRACE_DWC3_RESET_BEGIN, 0x4C435253); // "LCRS"
            if !super::platform::bramble::android_controller_block_reset() {
                log_puts("usb: SS post-Run/Stop Android link-clock reset failed\n");
            }
        }
        #[cfg(any(
            fullerene_aarch64_usb_gadget_handoff_ss_reassert_core_clocks_after_runstop,
            fullerene_aarch64_usb_gadget_handoff_ss_reassert_domain_after_runstop
        ))]
        if super_speed {
            // The stage-21 domain readouts show that this platform can drop
            // the USB30 domain at the Run/Stop boundary even after the same
            // vote/branch sequence was applied before it.  Keep both tests
            // opt-in and immediately after the production transition so
            // they change only domain persistence, before any SS snapshot or
            // endpoint/packet activity.
            if cfg!(fullerene_aarch64_usb_gadget_handoff_ss_reassert_domain_after_runstop) {
                // This is the full Android-style keepalive prefix: resend
                // the CX/BCM and regulator votes first, then restore GDSC,
                // RCG sources, and controller branches.
                let votes = super::platform::bramble::refresh_usb_domain_votes(
                    super::platform::bramble::UsbBusVote::Nominal,
                    true,
                );
                let controller = reassert_ss_controller_domain();
                log_hex(
                    "usb: SS post-Run/Stop domain refresh mask=",
                    u64::from((votes as u8) | ((controller as u8) << 1)),
                );
            } else {
                // Narrow control: no regulator re-send, only the controller
                // domain operation used by the earlier A/B.
                let _ = reassert_ss_controller_domain();
            }
        }
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if super_speed
            && option_env!("FULLERENE_USB_SIGNAL_CMD_GATE")
                .filter(|value| value.starts_with("ss-"))
                .is_some()
        {
            // Stage 20 is the pre-Run/Stop snapshot.  Capture again here,
            // immediately after the production transition, so stage 21
            // readouts classify the running USB31 link rather than the
            // expected pre-start SS_DIS state.  Keep this before the optional
            // device-mode A/B, which is intentionally a separate mutation.
            SS_RUNSTOP_POST_DCTL = read(DCTL);
            SS_RUNSTOP_POST_DSTS = read(DSTS);
            capture_ss_state_snapshot();
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reassert_device_mode)]
        if super_speed {
            // The preceding pre-Run/Stop write did not change the post
            // snapshot (`ss-gctl=0`). Repeat the same narrow mode tail after
            // Run/Stop to distinguish a transition-time GCTL overwrite from
            // an unwritable/incorrect PRTCAPDIR field.
            configure_dwc3_device_mode();
        }
        if super_speed && cfg!(fullerene_aarch64_usb_ep0_signal_probe) {
            if SS_RUNSTOP_POST_DCTL == 0xffff_ffff {
                SS_RUNSTOP_POST_DCTL = read(DCTL);
                SS_RUNSTOP_POST_DSTS = read(DSTS);
            }
        }
        RUN_STOP_TICK = arch_counter();
        // Stage 21 is immediately after the production Run/Stop transition
        // and its readback.  The remaining setup-arm/poll tail is untouched.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(21) {
            return true;
        }
        // Tight SETUP-arm window: retry the ep0 OUT Start Transfer every
        // 200 us for up to 100 ms after Run/Stop. The link reaches ON within
        // a few ms (the HS chirp handshake), and the host's first SETUP
        // token arrives only after its own attach debounce plus port reset -
        // arming in this window guarantees the first descriptor read is
        // answered instead of timing out (-110) while the poll-loop guard
        // was still waiting for the link state.
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup) {
            let arm_deadline =
                arch_counter().saturating_add(arch_counter_frequency().saturating_mul(100) / 1000);
            let mut armed = false;
            while arch_counter() < arm_deadline {
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
            trace_event(
                TRACE_SETUP_QUEUED,
                0x5441_524D, // "TARM" tight-arm outcome
                armed as u32,
                0,
                0,
                read(DSTS),
            );
            // Host-visible arm readout: a single DCTL.SDIS toggle right after
            // the window is one disconnect/re-attach pair in the host log.
            // It fires ONLY when the arm succeeded, which requires the core's
            // own link state to read ON (USBLNKST == 0) while the core is
            // running - so a pair at attach proves the core sees the HS link
            // in U0 and the -110 is in the EP0 data path (event ring / IN
            // DMA), while zero pairs means the core's link FSM never reached
            // U0 (PHY-level training the host saw, the core did not). SDIS on
            // a non-U0 link is host-invisible, so the absence is safe.
            if armed && option_env!("FULLERENE_USB_ARM_BLIP") == Some("1") {
                sdisc_blips(1);
            }
        } else {
            // Historical XBL differential marker. Source-guided Android/Linux
            // code eagerly arms CONTROL_SETUP; this flag is not a canonical
            // initial-SETUP model and must not suppress that baseline.
            trace_event(
                TRACE_SETUP_QUEUED,
                0x58424C44, // "XBLD": historical differential marker
                0,
                0,
                8,
                read(DSTS),
            );
        }

        // Stage 22 is after the tight post-Run/Stop SETUP-arm window, before
        // deferred post-Run/Stop probing and the C5 diagnostics.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(22) {
            return true;
        }

        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if option_env!("FULLERENE_USB_SIGNAL_DMA_POST_RUNSTOP") == Some("1") {
            // Defer the probe to the polling owner so the U0 check observes
            // the host-facing link rather than the pre-attach Run/Stop tail.
            POST_RUNSTOP_PROBE_PENDING = true;
            POST_RUNSTOP_PROBE_NOT_BEFORE = arch_counter().saturating_add(
                arch_counter_frequency().saturating_mul(POST_RUNSTOP_PROBE_DELAY_SECS),
            );
        }

        // C5: Run/Stop active and the tight SETUP-arm window is done.
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();

        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        ep0_signal_early_drop_check();

        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        ep0_signal_pre_runstop_drop_check();

        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        ep0_signal_heartbeat_check();

        // Stage 23 is after all C5 post-Run/Stop diagnostics, immediately
        // before the optional start-after-connect event-ring poll.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(23) {
            return true;
        }

        #[cfg(fullerene_aarch64_usb_gadget_handoff_start_after_connect)]
        {
            // Do NOT issue the pre-link-ON STARTTRANSFER here: on this core a
            // Start Transfer issued before the link reaches ON wedges the
            // endpoint command engine, so the host's first SETUP is never
            // serviced (descriptor read/64 -110) even though Run/Stop has
            // already published the pull-up. The SETUP TRB is prepared at
            // stage 6; the poll loop's U0-guarded try_arm_setup arms it the
            // moment the link comes ON - the same proven path the default
            // mode relies on. Consume any early event to keep the ring clean.
            poll_ep0_event_ring();
        }
        log_puts("usb: Fullerene DWC3 gadget connected\n");
        // C6: init tail done (start_after_connect arm + event poll); about to
        // return to the probe entry and cross the cfg-block boundary.
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();
        note_runtime_event(super::platform::bramble::UsbRuntimeEvent::ControllerStarted);

        // Stage 24 is the last line of init_with_super_speed(), after the
        // event-ring poll and runtime-start notification but before returning
        // to usb_probe's IRQ/ownership tail.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(24) {
            return true;
        }
    }
    true
}

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
    fullerene_aarch64_usb_gadget_handoff_gadget_restart_at_runstop
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
/// This diagnostic keeps the known direct-path speed and platform state. It
/// only repeats the DWC3 gadget-start command sequence: open a resource
/// window, allocate endpoint resources, initialize both physical EP0
/// directions, and arm the initial SETUP TRB. The caller deliberately
/// continues to the final Run/Stop transition when this best-effort restart
/// is incomplete, matching Android's void `dwc3_gadget_restart()` contract.
#[cfg(fullerene_aarch64_usb_gadget_handoff_gadget_restart_at_runstop)]
unsafe fn restart_gadget_at_runstop() -> bool {
    unsafe {
        republish_ep0_event_ring_at_runstop();
        configure_gadget_start_defaults();
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
            // Android's start_config() assigns one resource to every
            // hardware endpoint before either EP0 SETEPCONFIG command.
            for endpoint in 0..32usize {
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
        // to 64 bytes from Connect Done for a USB2 link. Keep that exact
        // initial packet size so this A/B includes the endpoint-start state,
        // while the existing event path remains responsible for the later
        // speed-specific modification.
        let epcfg0 = configure_endpoint_config(
            0,
            INITIAL_EP0_MAX_PACKET_SIZE,
            DEPCFG_EP_TYPE_CONTROL,
            false,
            0,
        );
        let epcfg1 = epcfg0
            && configure_endpoint_config(
                1,
                INITIAL_EP0_MAX_PACKET_SIZE,
                DEPCFG_EP_TYPE_CONTROL,
                false,
                0,
            );
        if !epcfg0 || !epcfg1 {
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
        ENDPOINTS_READY = true;
        let _ = udc_mut().configure_endpoint(0, INITIAL_EP0_MAX_PACKET_SIZE as u16, false);
        let _ = udc_mut().configure_endpoint(1, INITIAL_EP0_MAX_PACKET_SIZE as u16, false);
        write(DALEPENA, 0b11);
        let armed = start_setup();
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
        armed
    }
}

unsafe fn poll_ep0_event_ring() -> bool {
    if cfg!(fullerene_aarch64_usb_abl_event_consume) {
        return poll_ep0_event_ring_abl_style();
    }
    let count_register = unsafe { read(GEVNTCOUNT0) };
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
    let first_event = (event_base as *const u8).wrapping_add(first_offset);
    let first_word = unsafe {
        u32::from_le_bytes([
            read_volatile(first_event),
            read_volatile(first_event.add(1)),
            read_volatile(first_event.add(2)),
            read_volatile(first_event.add(3)),
        ])
    };
    let first_dsts = unsafe { read(DSTS) };
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
        // event-buffer handler's ordering.
        write(GEVNTSIZ0, event_size as u32 & GEVNTSIZ_SIZE_MASK);
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
        // The core retires a TRB by clearing HWO over DMA. Invalidate the
        // cached line first so the CPU observes the controller's write.
        let trb = ep0_trb_ptr(0);
        cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
        if read_volatile(addr_of!((*trb).ctrl)) & TRB_HWO == 0 {
            SIGNAL_SETUP_TRB_RETIRED = true;
        }
        let setup = ep0_setup_data_ptr() as *const u8;
        cache_invalidate(setup as usize, 8);
        for offset in 0..8 {
            if read_volatile(setup.add(offset)) != 0 {
                SIGNAL_SETUP_PACKET_RECEIVED = true;
                break;
            }
        }
        // DSTS_HIGHSPEED is zero, so the link state cannot be read from
        // ConnectSpd. A changing SOF frame number instead proves the core is
        // receiving packets from the host at the transaction level.
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
            13 => SIGNAL_LNKST_RESET = true,  // RESET: bus reset observed
            _ => {}
        }
        if dsts & DSTS_DEVCTRLHLT != 0 || read(DCTL) & DCTL_RUN_STOP == 0 {
            // A halted core or a cleared Run/Stop after a verified start makes
            // the physical attach a QSCRATCH session-override phantom.
            SIGNAL_CORE_HALTED = true;
        }
    }
}

/// Encode the polled EP0/DMA observables as one host-visible code. The probe
/// drops the physical pull-up `3 * code` seconds after attach, so the host
/// dmesg delta between "new high-speed USB device" and "USB disconnect" names
/// the first stage that provably worked:
///   1 = event ring delivered a record to GEVNTCOUNT
///   2 = DWC3 retired the armed EP0 SETUP TRB (HWO cleared over DMA)
///   3 = the SETUP packet payload was DMAed into the setup buffer
///   5 = SOF frames are arriving (transaction-level RX alive)
///   0 = none of the above (no drop; the host only sees its own -110)
/// SMMU read-only probe codes are handled by `probe_smmu_stream_state()`.
pub fn ep0_signal_code() -> u32 {
    unsafe {
        update_signal_latches();
        if SIGNAL_EVENT_DELIVERED {
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
///   1 = event ring delivered a record (GEVNTCOUNT > 0)
///   2 = the armed EP0 SETUP TRB was retired (HWO cleared over DMA)
///   3 = the SETUP packet payload was DMAed into the setup buffer
///   5 = SOF frame numbers are changing (transaction-level RX alive)
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
fn ep0_signal_early_drop_check() {
    let condition = match option_env!("FULLERENE_USB_SIGNAL_EARLY_DROP") {
        Some("1") => 1,
        Some("2") => 2,
        Some("3") => 3,
        Some("5") => 5,
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
        // dead EP0 from normal pre-attach delay.
        while ms < 20_000 {
            ms += 1;
            if condition != 9 {
                // Consume any pending events first: the delivery latch is
                // only set by a real event-ring poll.
                poll_ep0_event_ring();
                update_signal_latches();
                // Select the requested latch directly. Event delivery is a
                // prerequisite for the later observations, so a priority
                // ladder here would make code 2/3/5 impossible whenever the
                // reset or Connect Done event had already arrived.
                observed = match condition {
                    1 if SIGNAL_EVENT_DELIVERED => 1,
                    2 if SIGNAL_SETUP_TRB_RETIRED => 2,
                    3 if SIGNAL_SETUP_PACKET_RECEIVED => 3,
                    5 if SIGNAL_SOF_SEEN => 5,
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
            // Post-Run/Stop event-DMA differential: bit 0 = STARTTRANSFER
            // returned, bit 1 = ENDTRANSFER returned, bit 2 = the event ring
            // received a word. The gate is useful only when the optional
            // probe was compiled in; a missing record is false.
            "post" => TRACE_HARVEST_POST == 0x1_0007,
            "post-start" => {
                TRACE_HARVEST_POST != 0xFFFF_FFFF && TRACE_HARVEST_POST & 1 != 0
            }
            "post-end" => {
                TRACE_HARVEST_POST != 0xFFFF_FFFF && TRACE_HARVEST_POST & 2 != 0
            }
            "post-event" => {
                TRACE_HARVEST_POST != 0xFFFF_FFFF && TRACE_HARVEST_POST & 4 != 0
            }
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
/// log. This is the one-bit gate-TRUE readout; the SDIS blips and the
/// QSCRATCH drop are both dead channels here. The wait acknowledges device
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
/// status code is kept for the retained trace and the DCTL.SDIS blip
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
        write(
            DEVTEN,
            DEVTEN_DISCONNECT
                | DEVTEN_USB_RESET
                | DEVTEN_CONNECT_DONE
                | DEVTEN_LINK_STATUS_CHANGE
                | DEVTEN_WAKEUP
                | DEVTEN_HIBERNATION_REQUEST
                | DEVTEN_SUSPEND
                | DEVTEN_ERRATIC_ERROR
                | DEVTEN_CMD_COMPLETE
                | DEVTEN_OVERFLOW,
        );
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
    if option_env!("FULLERENE_USB_U0_ARM_PROBE")
        .filter(|value| *value != "0")
        .is_none()
    {
        return;
    }
    unsafe {
        U0_BLIP_PENDING = count.min(6);
    }
}

/// Queue the link-ON blip for the direct handoff success path. The
/// init-window blip only fires when the arm lands inside 100 ms of
/// Run/Stop, but the host's U0 arrives right at that window's edge, so a
/// zero there is not a discriminator. The poll loop's try_u0_blip is the
/// reliable readout: one SDIS disconnect/re-attach pair when the core
/// first reads the link ON, on whichever branch owns the poll loop.
pub fn arm_blip_queue() {
    if option_env!("FULLERENE_USB_ARM_BLIP")
        .filter(|value| *value != "0")
        .is_none()
    {
        return;
    }
    unsafe {
        U0_BLIP_PENDING = U0_BLIP_PENDING.max(1);
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
        if dsts & DSTS_DEVCTRLHLT != 0 || (dsts >> 18) & 0xf != 0 {
            return;
        }
        let count = U0_BLIP_PENDING;
        U0_BLIP_PENDING = 0;
        sdisc_blips(count);
    }
}

/// Emit up to `count` SDIS blips only when the core reports the link ON
/// (running, unhalted, USBLNKST == 0). On a non-U0 link the soft
/// disconnect is host-invisible, so this is a silent no-op before attach.
pub fn sdisc_blips_link_on(count: u32) {
    unsafe {
        let dsts = read(DSTS);
        if dsts & DSTS_DEVCTRLHLT != 0 || (dsts >> 18) & 0xf != 0 {
            return;
        }
        sdisc_blips(count);
    }
}

/// Host-visible failure readout: toggle the core's own soft disconnect
/// (`DCTL.SDIS`) `count` times. Each toggle is one disconnect/re-attach
/// pair in the host kernel log, so the LAST run's pair count names which
/// recovery step failed without any other channel (timing readouts are
/// masked by the secure watchdog, and the QSCRATCH session overrides are
/// host-invisible on this board). A core that never reached Run/Stop
/// ignores DCTL, so ZERO blips is itself the "core not running" signal.
pub fn sdisc_blips(count: u32) {
    unsafe {
        for _ in 0..count.min(6) {
            let mut dctl = read(DCTL);
            dctl |= DCTL_SDIS;
            write_dctl_safe(dctl);
            super::timer::delay_ms(300);
            dctl = read(DCTL);
            dctl &= !DCTL_SDIS;
            write_dctl_safe(dctl);
            super::timer::delay_ms(200);
        }
    }
}

/// Fast SDIS blip variant for the "diag" gate readout: 100 ms disconnect +
/// 70 ms reconnect per pair (170 ms total), so the control pair plus all
/// six code pairs finish in ~1.2 s. The gate evaluates at ~T+15.3 (4 s
/// observation window after the ~T+11.3 post-init probe entry) and the last
/// re-attach must land before the ~T+17-18 secure-WDT bite. Same link-ON
/// guard as `sdisc_blips_link_on`: a core that is not running (or whose
/// link is down) ignores DCTL, so ZERO pairs in the host journal is itself
/// the "no live core at eval" readout.
pub fn sdisc_blips_fast(count: u32) {
    unsafe {
        let dsts = read(DSTS);
        if dsts & DSTS_DEVCTRLHLT != 0 || (dsts >> 18) & 0xf != 0 {
            return;
        }
        for _ in 0..count.min(6) {
            let mut dctl = read(DCTL);
            dctl |= DCTL_SDIS;
            write_dctl_safe(dctl);
            super::timer::delay_ms(100);
            dctl = read(DCTL);
            dctl &= !DCTL_SDIS;
            write_dctl_safe(dctl);
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
        link_on_sample();
        // Diagnostic quiet window (see mmio_quiet_active): after this many
        // seconds past the first Run/Stop, stop ALL controller MMIO access.
        if mmio_quiet_active() {
            return;
        }
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
        let event_seen = poll_ep0_event_ring();
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
            // start/end/event bits even when the core refuses the synthetic
            // command.
            POST_RUNSTOP_PROBE_PENDING = false;
            let _ = post_runstop_event_dma_probe();
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
