//! Retained USB trace ABI and host-visible trace APIs.

use super::super::usb_protocol::{
    TRACE_CONTROL_ENTRY_BYTES, TRACE_CONTROL_HEADER_BYTES, TRACE_CONTROL_PAGE_ENTRIES,
};
use super::{EP0_STATE, Ep0State};
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

pub(crate) const USB_TRACE_CAPACITY: usize = 256;

// Numeric events keep the early USB path independent of UART, locks, and
// formatting. The buffer is CPU-owned; it is placed beside the DMA objects so
// a probe can preserve the same identity-mapped address discipline.
pub(crate) const TRACE_INIT: u32 = 1;
pub(crate) const TRACE_DEVICE_RESET: u32 = 2;
pub(crate) const TRACE_DEVICE_CONNECT: u32 = 3;
pub(crate) const TRACE_EP_COMMAND_ISSUE: u32 = 4;
pub(crate) const TRACE_EP_COMMAND_DONE: u32 = 5;
pub(crate) const TRACE_EP_COMMAND_TIMEOUT: u32 = 6;
pub(crate) const TRACE_SETUP_QUEUED: u32 = 7;
pub(crate) const TRACE_SETUP_RECEIVED: u32 = 8;
pub(crate) const TRACE_DESCRIPTOR_QUEUED: u32 = 9;
pub(crate) const TRACE_STATUS_QUEUED: u32 = 10;
pub(crate) const TRACE_TRANSFER_COMPLETE: u32 = 11;
pub(crate) const TRACE_USB_RESET: u32 = 12;
pub const TRACE_BOOT_USB_ENTRY: u32 = 13;
pub const TRACE_TYPEC_BEGIN: u32 = 14;
pub const TRACE_TYPEC_DONE: u32 = 15;
pub const TRACE_USB_HANDOFF_BEGIN: u32 = 16;
pub(crate) const TRACE_DWC3_RESET_BEGIN: u32 = 17;
pub(crate) const TRACE_QSCRATCH_BEGIN: u32 = 18;
pub const TRACE_EXCEPTION_SYNC: u32 = 19;
pub const TRACE_PROBE_WATCHDOG: u32 = 33;
pub(crate) const TRACE_LINK_STATUS: u32 = 20;
pub(crate) const TRACE_USB_WAKEUP: u32 = 21;
pub(crate) const TRACE_USB_SUSPEND: u32 = 22;
pub(crate) const TRACE_USB_DEVICE_ERROR: u32 = 23;
pub const TRACE_TYPEC_EVENT: u32 = 24;
pub const TRACE_PLATFORM_IRQ: u32 = 25;
pub const TRACE_UDC_REARM: u32 = 26;
pub(crate) const TRACE_SMMU_BEGIN: u32 = 27;
pub(crate) const TRACE_SMMU_READY: u32 = 28;
pub(crate) const TRACE_SMMU_HANDOFF: u32 = 34;
pub(crate) const TRACE_SMMU_PRESERVED: u32 = 35;
pub(crate) const TRACE_SMMU_FAULT: u32 = 36;
pub(crate) const TRACE_SMMU_GLOBAL_FAULT: u32 = 37;
pub(crate) const TRACE_UTMI_CLOCK: u32 = 29;
pub(crate) const TRACE_EVENT_RING_READY: u32 = 30;
pub(crate) const TRACE_DWC3_HALTED: u32 = 31;
pub(crate) const TRACE_DWC3_HALT_TIMEOUT: u32 = 32;
pub(crate) const TRACE_DWC3_REVISION_QUIRK: u32 = 38;
pub(crate) const TRACE_XFER_NOT_READY: u32 = 40;
pub(crate) const TRACE_GCC_UTMI_CLOCK: u32 = 41;
pub(crate) const TRACE_USB2_PHY_RESET: u32 = 42;
/// Snapshot of the DWC3 USB2 PHY and Bramble GCC clock state at a handoff
/// boundary. The payload is intentionally raw so a later enumerated trace
/// read can compare source/divider and power-state bits without relying on
/// UART output from the temporary image.
pub(crate) const TRACE_UTMI_STATE: u32 = 43;
/// Gate-evaluation snapshot of the DWC3 protocol boundary. Groups 0..2 carry
/// the event-count/device registers, endpoint command state, and EP0 TRB/setup
/// ownership respectively. It is deliberately separate from TRACE_UTMI_STATE
/// so a protocol-error readout cannot be mistaken for a PHY clock sample.
pub(crate) const TRACE_DWC3_BOUNDARY: u32 = 44;

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct UsbTraceEntry {
    pub(crate) sequence: u32,
    pub(crate) event: u32,
    pub(crate) request: u32,
    pub(crate) value: u32,
    pub(crate) index: u32,
    pub(crate) length: u32,
    pub(crate) ep0_state: u32,
    pub(crate) status: u32,
}

pub(crate) const EMPTY_USB_TRACE: UsbTraceEntry = UsbTraceEntry {
    sequence: 0,
    event: 0,
    request: 0,
    value: 0,
    index: 0,
    length: 0,
    ep0_state: 0,
    status: 0,
};

pub(crate) const USB_TRACE_MAGIC: u32 = 0x4655_5452; // "FUTR"
pub(crate) const USB_TRACE_VERSION: u32 = 1;

#[repr(C, align(4096))]
pub(crate) struct UsbTraceBuffer {
    pub(crate) magic: u32,
    pub(crate) version: u32,
    pub(crate) head: u32,
    pub(crate) reserved: u32,
    pub(crate) entries: [UsbTraceEntry; USB_TRACE_CAPACITY],
}

#[unsafe(link_section = ".usb_trace")]
pub(crate) static mut USB_TRACE: UsbTraceBuffer = UsbTraceBuffer {
    magic: 0,
    version: 0,
    head: 0,
    reserved: 0,
    entries: [EMPTY_USB_TRACE; USB_TRACE_CAPACITY],
};

// The retained trace can be scribbled by the stock Android takeover before a
// later boot reads it. Keep a current-boot copy for the UTMI readout gate so
// that a register value is not confused with a missing retained record.
static mut LIVE_UTMI_GUSB2: [u32; 16] = [0; 16];
static mut LIVE_UTMI_VALID: u16 = 0;
// The stage snapshots are taken at named handoff boundaries. Keep the
// write/readback pair separately as well: a zero stage value can mean either
// that the GUSB2PHYCFG write was ignored immediately or that a later reset
// cleared it before the boundary snapshot.
static mut LIVE_UTMI_WRITE_REQUESTED: u32 = 0;
static mut LIVE_UTMI_WRITE_READBACK: u32 = 0;
static mut LIVE_UTMI_WRITE_VALID: bool = false;
/// DALEPENA readback immediately after the final DCTL Run/Stop write. This
/// separates an accepted post-Run/Stop EP0 enable from a later USB reset or
/// link transition that clears the endpoint-enable mask.
static mut LIVE_DALEPENA_AFTER_DCTL: u32 = 0;
static mut LIVE_DALEPENA_VALID: bool = false;
/// DALEPENA readback immediately before the final DCTL Run/Stop write.
static mut LIVE_DALEPENA_BEFORE_DCTL: u32 = 0;
static mut LIVE_DALEPENA_BEFORE_VALID: bool = false;
/// DALEPENA readback before and after the opt-in USB-reset re-publication.
/// The low nibble is the pre-write value; the high nibble is the post-write
/// value. Keeping both sides distinguishes a reset-cleared mask from a
/// write that the controller refuses at that boundary.
static mut LIVE_DALEPENA_AFTER_RESET: u32 = 0;
static mut LIVE_DALEPENA_AFTER_RESET_VALID: bool = false;
/// Packed DALEPENA readbacks after the EP0 OUT and EP0 IN configuration
/// publications. Each direction occupies two bits: OUT in bits 1:0 and IN
/// in bits 3:2. This is kept outside retained DRAM so the readback belongs to
/// the current handoff attempt.
static mut LIVE_DALEPENA_CONFIG: u32 = 0;
static mut LIVE_DALEPENA_CONFIG_VALID: u8 = 0;
/// First non-empty DWC3 EP0 event-buffer observation in this boot. Keep the
/// producer-side count and the consumer-side slot word together: a non-zero
/// GEVNTCOUNT with a zero slot is a DMA/cache/ownership failure, while a
/// non-zero slot proves that the controller produced an event before the
/// software consumer handled it.
static mut LIVE_DWC3_FIRST_EVENT_COUNT: u32 = 0;
static mut LIVE_DWC3_FIRST_EVENT_OFFSET: u32 = 0;
static mut LIVE_DWC3_FIRST_EVENT_WORD: u32 = 0;
static mut LIVE_DWC3_FIRST_EVENT_DSTS: u32 = 0;
static mut LIVE_DWC3_FIRST_EVENT_DCTL: u32 = 0;
static mut LIVE_DWC3_FIRST_EVENT_VALID: bool = false;

/// Initialize the retained trace header and append a boot boundary marker.
/// The entry array is intentionally not cleared, so a subsequent boot can
/// inspect the last attempt after a warm reset.
pub(super) fn trace_begin() {
    unsafe {
        let magic = read_volatile(addr_of!(USB_TRACE).cast::<u32>());
        let version = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(1));
        if magic != USB_TRACE_MAGIC || version != USB_TRACE_VERSION {
            write_volatile(addr_of_mut!(USB_TRACE).cast::<u32>(), USB_TRACE_MAGIC);
            write_volatile(
                addr_of_mut!(USB_TRACE).cast::<u32>().add(1),
                USB_TRACE_VERSION,
            );
            write_volatile(addr_of_mut!(USB_TRACE).cast::<u32>().add(2), 0);
            write_volatile(addr_of_mut!(USB_TRACE).cast::<u32>().add(3), 0);
        }
    }
    trace_event(TRACE_BOOT_USB_ENTRY, 0, 0, 0, 0, 0);
}

#[inline(always)]
fn ep0_state_code(state: Ep0State) -> u32 {
    match state {
        Ep0State::Setup => 1,
        Ep0State::Data => 2,
        Ep0State::Status => 3,
    }
}

#[inline(always)]
pub(super) fn trace_event(
    event: u32,
    request: u32,
    value: u32,
    index: u32,
    length: u32,
    status: u32,
) {
    unsafe {
        let head_ptr = addr_of_mut!(USB_TRACE).cast::<u32>().add(2);
        let head = read_volatile(head_ptr);
        let slot = (head as usize) % USB_TRACE_CAPACITY;
        let entry = UsbTraceEntry {
            sequence: head.wrapping_add(1),
            event,
            request,
            value,
            index,
            length,
            ep0_state: ep0_state_code(EP0_STATE),
            status,
        };
        write_volatile(
            addr_of_mut!(USB_TRACE.entries)
                .cast::<UsbTraceEntry>()
                .add(slot),
            entry,
        );
        write_volatile(head_ptr, head.wrapping_add(1));
    }
}

/// Start a retained trace for the standalone handoff probe without clearing
/// the previous attempt. A subsequent normal Fullerene boot can dump it over
/// UART before starting a new trace.
pub fn trace_probe_begin() {
    trace_begin();
}

/// Reset the retained trace cursor for a fresh boot. The region survives
/// warm resets by design, but between two `fastboot boot` runs Android
/// scribbles DRAM unpredictably: a surviving header would make the in-boot
/// harvest gates count the PREVIOUS run's records. The probe calls this once
/// per boot, before the first handoff attempt, so attempts 2/3 still see
/// attempt 1/2's records while cross-boot contamination is impossible.
pub fn trace_reset_head_for_boot() {
    unsafe {
        write_volatile(addr_of_mut!(USB_TRACE).cast::<u32>().add(2), 0);
        LIVE_UTMI_VALID = 0;
        LIVE_UTMI_WRITE_VALID = false;
        LIVE_DALEPENA_VALID = false;
        LIVE_DALEPENA_BEFORE_VALID = false;
        LIVE_DALEPENA_AFTER_RESET = 0;
        LIVE_DALEPENA_AFTER_RESET_VALID = false;
        LIVE_DALEPENA_CONFIG = 0;
        LIVE_DALEPENA_CONFIG_VALID = 0;
        LIVE_DWC3_FIRST_EVENT_COUNT = 0;
        LIVE_DWC3_FIRST_EVENT_OFFSET = 0;
        LIVE_DWC3_FIRST_EVENT_WORD = 0;
        LIVE_DWC3_FIRST_EVENT_DSTS = 0;
        LIVE_DWC3_FIRST_EVENT_DCTL = 0;
        LIVE_DWC3_FIRST_EVENT_VALID = false;
        core::arch::asm!("dsb sy", options(nostack));
    }
}

/// Save one current-boot UTMI snapshot outside the retained DRAM channel.
/// This is intentionally volatile-state-free: it is consumed only before the
/// same boot resets, when the diagnostic gate publishes the value.
pub(super) fn live_utmi_stage(stage: u32, gusb2: u32) {
    if stage < 16 {
        unsafe {
            LIVE_UTMI_GUSB2[stage as usize] = gusb2;
            LIVE_UTMI_VALID |= 1 << stage;
        }
    }
}

/// Preserve the immediate GUSB2PHYCFG write/readback pair in current-boot
/// state. The retained trace may be scribbled by Android before a later
/// readout, so this diagnostic must remain outside that channel.
pub(super) fn live_utmi_write(requested: u32, readback: u32) {
    unsafe {
        LIVE_UTMI_WRITE_REQUESTED = requested;
        LIVE_UTMI_WRITE_READBACK = readback;
        LIVE_UTMI_WRITE_VALID = true;
    }
}

/// Preserve the EP0 endpoint-enable readback immediately after Run/Stop.
pub(super) fn live_dalepena_after_dctl(readback: u32) {
    unsafe {
        LIVE_DALEPENA_AFTER_DCTL = readback;
        LIVE_DALEPENA_VALID = true;
    }
}

pub(super) fn live_dalepena_before_dctl(readback: u32) {
    unsafe {
        LIVE_DALEPENA_BEFORE_DCTL = readback;
        LIVE_DALEPENA_BEFORE_VALID = true;
    }
}

pub(super) fn live_dalepena_after_reset(before: u32, after: u32) {
    unsafe {
        LIVE_DALEPENA_AFTER_RESET = (before & 0xf) | ((after & 0xf) << 4);
        LIVE_DALEPENA_AFTER_RESET_VALID = true;
    }
}

/// Preserve the DALEPENA readback immediately after one EP0 direction is
/// published. The `dwc3-dale-config` selector packs both readbacks into one
/// host-visible nibble: OUT bits 1:0, IN bits 3:2.
pub(super) fn live_dalepena_config(direction: u32, readback: u32) {
    if direction < 2 {
        unsafe {
            let shift = direction * 2;
            LIVE_DALEPENA_CONFIG =
                (LIVE_DALEPENA_CONFIG & !(0x3 << shift)) | ((readback & 0x3) << shift);
            LIVE_DALEPENA_CONFIG_VALID |= 1 << direction;
        }
    }
}

/// Save the first producer/consumer boundary without touching the event
/// count. Returns true only for the first observation so the retained trace
/// gets one stable record rather than a stream of polling duplicates.
pub(super) fn live_dwc3_first_event(
    count: u32,
    offset: u32,
    word: u32,
    dsts: u32,
    dctl: u32,
) -> bool {
    unsafe {
        if LIVE_DWC3_FIRST_EVENT_VALID {
            return false;
        }
        LIVE_DWC3_FIRST_EVENT_COUNT = count;
        LIVE_DWC3_FIRST_EVENT_OFFSET = offset;
        LIVE_DWC3_FIRST_EVENT_WORD = word;
        LIVE_DWC3_FIRST_EVENT_DSTS = dsts;
        LIVE_DWC3_FIRST_EVENT_DCTL = dctl;
        LIVE_DWC3_FIRST_EVENT_VALID = true;
        true
    }
}

/// Classify the previous boot's retained enumeration progress into an
/// attach-delay readout code. Must be called before the per-boot cursor
/// reset, so every record belongs to the previous boot: the region is NOLOAD
/// and warm-reset retained, and the cursor restarts at 1 each boot, so a
/// valid trace has entry i carrying sequence i+1 (the boot marker written
/// before the reset is orphaned at its slot and must not be required). The
/// following boot delays its physical attach by `code` seconds (on top
/// of the PON delay), so the host journal's attach timestamp publishes how
/// far the previous enumeration progressed:
///   0 = no verifiable retained trace (first boot, scribbled DRAM, or an
///       empty cursor - a gate-suppressed boot writes nothing after the
///       reset, so its trace reads back as 0)
///   1 = records exist but no SETUP ever reached EP0
///   2 = a SETUP was received by software
///   3 = a descriptor data TRB was queued (the data phase armed)
///   4 = XferNotReady(CONTROL_DATA) arrived on EP1 IN
///   5 = an EP1 IN transfer completed on the wire
///   6 = a SET_ADDRESS was received (the host accepted the descriptor)
pub fn prev_boot_progress_code() -> u32 {
    unsafe {
        let magic = read_volatile(addr_of!(USB_TRACE).cast::<u32>());
        let version = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(1));
        if magic != USB_TRACE_MAGIC || version != USB_TRACE_VERSION {
            return 0;
        }
        let head = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(2));
        if head == 0 || head as usize > USB_TRACE_CAPACITY {
            return 0;
        }
        let count = head as usize;
        for index in 0..count {
            let entry = addr_of!(USB_TRACE.entries)
                .cast::<UsbTraceEntry>()
                .add(index);
            if read_volatile(addr_of!((*entry).sequence)) != (index + 1) as u32 {
                return 0;
            }
        }
        let mut setup = 0u32;
        let mut descriptor = 0u32;
        let mut ep1_nrdy = 0u32;
        let mut ep1_complete = 0u32;
        let mut set_address = 0u32;
        for index in 0..count {
            let entry = addr_of!(USB_TRACE.entries)
                .cast::<UsbTraceEntry>()
                .add(index);
            let event = read_volatile(addr_of!((*entry).event));
            match event {
                TRACE_SETUP_RECEIVED => {
                    setup += 1;
                    if read_volatile(addr_of!((*entry).request)) == 5 {
                        set_address += 1;
                    }
                }
                TRACE_DESCRIPTOR_QUEUED => descriptor += 1,
                TRACE_XFER_NOT_READY => {
                    if read_volatile(addr_of!((*entry).request)) == 1
                        && read_volatile(addr_of!((*entry).value)) == 1
                    {
                        ep1_nrdy += 1;
                    }
                }
                TRACE_TRANSFER_COMPLETE => {
                    if read_volatile(addr_of!((*entry).request)) == 1
                        && read_volatile(addr_of!((*entry).value)) == 1
                    {
                        ep1_complete += 1;
                    }
                }
                _ => {}
            }
        }
        if set_address > 0 {
            6
        } else if ep1_complete > 0 {
            5
        } else if ep1_nrdy > 0 {
            4
        } else if descriptor > 0 {
            3
        } else if setup > 0 {
            2
        } else {
            1
        }
    }
}

/// Classify the previous boot at the protocol boundary for A/B gates. This
/// intentionally reports only the two milestones that are useful when the
/// ordinary progress code is still `1`: an `ARME` record proves that the EP0
/// SETUP Start Transfer command completed, while a Connect Done record proves
/// that the DWC3 link FSM reached the device-connect event. A SETUP received
/// by software takes precedence over both because it is already a later,
/// stronger milestone.
///
///   0 = no verifiable retained trace
///   1 = valid trace, but neither marker was retained
///   4 = EP0 SETUP transfer was armed, but no SETUP was received
///   5 = Connect Done was observed, but no SETUP was received
///   6 = a SETUP was received (the later progress ladder is available)
pub fn prev_boot_boundary_code() -> u32 {
    unsafe {
        let magic = read_volatile(addr_of!(USB_TRACE).cast::<u32>());
        let version = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(1));
        if magic != USB_TRACE_MAGIC || version != USB_TRACE_VERSION {
            return 0;
        }
        let head = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(2));
        if head == 0 || head as usize > USB_TRACE_CAPACITY {
            return 0;
        }
        let count = head as usize;
        let mut armed = false;
        let mut connected = false;
        let mut setup = false;
        for index in 0..count {
            let entry = addr_of!(USB_TRACE.entries)
                .cast::<UsbTraceEntry>()
                .add(index);
            if read_volatile(addr_of!((*entry).sequence)) != (index + 1) as u32 {
                return 0;
            }
            match read_volatile(addr_of!((*entry).event)) {
                TRACE_SETUP_QUEUED if read_volatile(addr_of!((*entry).request)) == 0x4152_4D45 => {
                    armed = true;
                }
                TRACE_DEVICE_CONNECT => connected = true,
                TRACE_SETUP_RECEIVED => setup = true,
                _ => {}
            }
        }
        if setup {
            6
        } else if armed {
            4
        } else if connected {
            5
        } else {
            1
        }
    }
}

/// Classify the deepest QMP initialization marker retained by the previous
/// boot. This is intentionally a coarse phase code: the host-side loop cannot
/// read the marker buffer before the temporary image enumerates, but it can
/// use the next boot's attach/no-attach result as a one-bit threshold test.
///
///   0 = no valid retained QMP marker
///   1 = entered `init_qmp_phy()` (`QMPB`)
///   2 = control preamble (`QMCP`)
///   3 = QMP table started (`QMTB` or a table-entry marker)
///   4 = table completed (`QMTE`)
///   5 = PCS start boundary (`QMST`)
///   6 = first PCS status read (`QMSR`)
///   7 = status poll (`QMPL`)
///   8 = PHY ready (`QMOK`)
pub fn prev_boot_qmp_phase_code() -> u32 {
    unsafe {
        let magic = read_volatile(addr_of!(USB_TRACE).cast::<u32>());
        let version = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(1));
        if magic != USB_TRACE_MAGIC || version != USB_TRACE_VERSION {
            return 0;
        }
        let head = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(2));
        if head == 0 || head as usize > USB_TRACE_CAPACITY {
            return 0;
        }
        let mut phase = 0u32;
        for index in 0..head as usize {
            let entry = addr_of!(USB_TRACE.entries)
                .cast::<UsbTraceEntry>()
                .add(index);
            if read_volatile(addr_of!((*entry).sequence)) != (index + 1) as u32 {
                return 0;
            }
            if read_volatile(addr_of!((*entry).event)) != TRACE_PROBE_WATCHDOG {
                continue;
            }
            let marker = read_volatile(addr_of!((*entry).status));
            phase = phase.max(match marker {
                0x514d_5042 => 1, // QMPB
                0x514d_4350 => 2, // QMCP
                0x514d_5442 => 3, // QMTB
                0x514d_5445 => 4, // QMTE
                0x514d_5354 => 5, // QMST
                0x514d_5352 => 6, // QMSR
                0x514d_504c => 7, // QMPL
                0x514d_4f4b => 8, // QMOK
                value if value & 0xffff_ff00 == 0x514d_0000 => 3,
                _ => 0,
            });
        }
        phase
    }
}

/// Add a marker without touching the controller. This is used around PMIC
/// and platform transitions where the next MMIO access itself may abort.
pub fn trace_marker(event: u32, status: u32) {
    trace_event(event, 0, 0, 0, 0, status);
}

/// Read the retained trace cursor without changing the controller state.
/// Standalone probes use this as a watchdog activity signal: an EP0/device
/// event advances the cursor, while a completely absent USB session does not.
pub fn trace_head() -> u32 {
    unsafe { read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(2)) }
}

/// Read the last committed event from the retained trace without advancing
/// it. The serial-string transport uses this pair as a compact host-visible
/// snapshot when the gadget has enumerated but UART is unavailable.
pub fn trace_last_event() -> u32 {
    unsafe {
        let magic = read_volatile(addr_of!(USB_TRACE).cast::<u32>());
        let version = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(1));
        let head = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(2));
        if magic != USB_TRACE_MAGIC || version != USB_TRACE_VERSION || head == 0 {
            return 0;
        }
        let slot = (head.wrapping_sub(1) as usize) % USB_TRACE_CAPACITY;
        read_volatile(
            addr_of!(USB_TRACE.entries)
                .cast::<UsbTraceEntry>()
                .add(slot),
        )
        .event
    }
}

/// Encode one raw UTMI-facing register field for a host-visible readout.
///
/// The temporary image normally cannot enumerate far enough to expose the
/// retained trace over its vendor control request.  The signal probe uses
/// this small selector API before reset and publishes the returned nibble in
/// its reset delay.  Values are taken from the newest complete snapshot that
/// was appended by `trace_utmi_state()`; no register is written here.
pub(super) fn utmi_readout_code(selector: &str) -> u32 {
    unsafe {
        let live_stage = match selector {
            "utmi-trdtim-stage1" => Some(1usize),
            "utmi-trdtim-stage2" => Some(2usize),
            "utmi-trdtim-stage3" => Some(3usize),
            "utmi-trdtim-stage4" => Some(4usize),
            "utmi-trdtim-stage5" => Some(5usize),
            _ => None,
        };
        if let Some(stage) = live_stage {
            if LIVE_UTMI_VALID & (1 << stage) != 0 {
                return (LIVE_UTMI_GUSB2[stage] >> 10) & 0xf;
            }
        }
        if selector == "utmi-valid" && LIVE_UTMI_VALID != 0 {
            return 3;
        }
        if LIVE_UTMI_WRITE_VALID {
            match selector {
                "utmi-write-requested-trdtim" => {
                    return (LIVE_UTMI_WRITE_REQUESTED >> 10) & 0xf;
                }
                "utmi-write-readback-trdtim" => {
                    return (LIVE_UTMI_WRITE_READBACK >> 10) & 0xf;
                }
                _ => {}
            }
        }
        if selector == "dwc3-dale-after-dctl" && LIVE_DALEPENA_VALID {
            return LIVE_DALEPENA_AFTER_DCTL & 0xf;
        }
        if selector == "dwc3-dale-before-dctl" && LIVE_DALEPENA_BEFORE_VALID {
            return LIVE_DALEPENA_BEFORE_DCTL & 0xf;
        }
        if LIVE_DALEPENA_AFTER_RESET_VALID {
            if selector == "dwc3-dale-reset-seen" {
                return 1;
            }
            if selector == "dwc3-dale-reset-before" {
                return LIVE_DALEPENA_AFTER_RESET & 0xf;
            }
            if selector == "dwc3-dale-reset-after" {
                return (LIVE_DALEPENA_AFTER_RESET >> 4) & 0xf;
            }
        }
        if selector == "dwc3-dale-config" && LIVE_DALEPENA_CONFIG_VALID == 0x3 {
            return LIVE_DALEPENA_CONFIG & 0xf;
        }
        let magic = read_volatile(addr_of!(USB_TRACE).cast::<u32>());
        let version = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(1));
        let head = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(2));
        if magic != USB_TRACE_MAGIC || version != USB_TRACE_VERSION || head == 0 {
            return 0;
        }

        // The four records are emitted consecutively for each stage. Keep
        // the newest value for each group; a complete stage therefore wins
        // over earlier takeover snapshots even when the core later wedges.
        let valid = (head as usize).min(USB_TRACE_CAPACITY);
        let oldest = (head as usize).saturating_sub(valid);
        let mut gusb2 = 0u32;
        let mut utmi_source = 0u32;
        let mut utmi_branch = 0u32;
        let mut dsts = 0u32;
        let mut hs_ctrl0 = 0u32;
        let mut hs_ctrl2 = 0u32;
        let mut hs_ctrl5 = 0u32;
        let mut hs_qscratch = 0u32;
        let mut gusb2_by_stage = [0u32; 16];
        let mut seen = [false; 4];
        for offset in 0..valid {
            let slot = (oldest + offset) % USB_TRACE_CAPACITY;
            let entry = read_volatile(
                addr_of!(USB_TRACE.entries)
                    .cast::<UsbTraceEntry>()
                    .add(slot),
            );
            if entry.event != TRACE_UTMI_STATE {
                continue;
            }
            let group = ((entry.request >> 24) & 0x3) as usize;
            seen[group] = true;
            match group {
                0 => {
                    gusb2 = entry.value;
                    utmi_source = entry.length;
                    utmi_branch = entry.status;
                    let stage = (entry.request & 0xff) as usize;
                    if stage < gusb2_by_stage.len() {
                        gusb2_by_stage[stage] = entry.value;
                    }
                }
                2 => {
                    // Group 2 is the DWC3 USB3/link snapshot.  Although the
                    // failed handoff is USB2, DSTS.USBLNKST and
                    // DEVCTRLHLT are the controller's authoritative view of
                    // whether the device-side link FSM is running.
                    dsts = entry.status;
                }
                3 => {
                    // Group 3 is the raw Qualcomm HS-PHY snapshot:
                    // UTMI_CTRL0, CTRL2, UTMI_CTRL5, and HS PHY QSCRATCH.
                    hs_ctrl0 = entry.value;
                    hs_ctrl2 = entry.index;
                    hs_ctrl5 = entry.length;
                    hs_qscratch = entry.status;
                }
                _ => {}
            }
        }
        if selector.starts_with("dwc3-") {
            if selector == "dwc3-first-event" && LIVE_DWC3_FIRST_EVENT_VALID {
                // 1 = first raw word was a device event, 2 = endpoint event.
                // This is a compact same-boot readout; the retained trace
                // carries the full count/offset/raw/register values.
                return 1 + (LIVE_DWC3_FIRST_EVENT_WORD & 1);
            }
            let mut event_count = 0u32;
            let mut dsts = 0u32;
            let mut dctl = 0u32;
            let mut devten = 0u32;
            let mut dalepena = 0u32;
            let mut depcmd0 = 0u32;
            let mut depcmd1 = 0u32;
            let mut trb0 = 0u32;
            let mut trb1 = 0u32;
            let mut compact = 0u32;
            let mut dwc3_seen = [false; 3];
            for offset in 0..valid {
                let slot = (oldest + offset) % USB_TRACE_CAPACITY;
                let entry = read_volatile(
                    addr_of!(USB_TRACE.entries)
                        .cast::<UsbTraceEntry>()
                        .add(slot),
                );
                if entry.event != TRACE_DWC3_BOUNDARY {
                    continue;
                }
                let group = (entry.request & 0xff) as usize;
                if group >= dwc3_seen.len() {
                    continue;
                }
                dwc3_seen[group] = true;
                match group {
                    0 => {
                        event_count = entry.value;
                        dsts = entry.index;
                        dctl = entry.length;
                        devten = entry.status;
                    }
                    1 => {
                        dalepena = entry.value;
                        depcmd0 = entry.index;
                        depcmd1 = entry.length;
                        compact = entry.status;
                    }
                    2 => {
                        trb0 = entry.value;
                        trb1 = entry.index;
                    }
                    _ => {}
                }
            }
            if !dwc3_seen[0] || !dwc3_seen[1] {
                return 0;
            }
            return match selector {
                // Four-bit boundary code, suitable for the attach-cycle
                // readout: bit 0 event FIFO non-empty, bit 1 setup payload
                // non-zero, bit 2 setup TRB still owned by the core, bit 3 a
                // DWC3 device-error event was observed.
                "dwc3-state" => compact & 0xf,
                "dwc3-event" => ((event_count & 0xfffc) >> 2) & 0xf,
                "dwc3-link" => (dsts >> 18) & 0xf,
                "dwc3-run" => (dctl >> 31) & 1,
                "dwc3-devten" => devten & 0xf,
                "dwc3-dale" => dalepena & 0xf,
                "dwc3-cmd0" => (depcmd0 >> 12) & 0xf,
                "dwc3-cmd1" => (depcmd1 >> 12) & 0xf,
                "dwc3-cmd0-act" => (depcmd0 >> 10) & 1,
                "dwc3-cmd1-act" => (depcmd1 >> 10) & 1,
                "dwc3-trb" => (trb0 & 1) | ((trb1 & 1) << 1),
                _ => 0,
            };
        }
        if selector == "utmi-valid" {
            // Return a presence mask rather than a boolean so a zero raw
            // register value cannot be confused with a missing snapshot:
            // bit 0 = GUSB2 group, bit 1 = DSTS/link group, bit 2 = HS-PHY
            // group.
            return u32::from(seen[0]) | (u32::from(seen[2]) << 1) | (u32::from(seen[3]) << 2);
        }
        if selector == "hsphy-valid" {
            return u32::from(seen[3]);
        }
        if seen[3] {
            let value = match selector {
                "hsphy-sleepm" => hs_ctrl0 & 1,
                "hsphy-opmode" => (hs_ctrl0 >> 3) & 0x3,
                "hsphy-termsel" => (hs_ctrl0 >> 5) & 1,
                "hsphy-suspend-n" => (hs_ctrl2 >> 2) & 1,
                "hsphy-suspend-n-sel" => (hs_ctrl2 >> 3) & 1,
                "hsphy-por" | "hsphy-por-clear-after-runstop" => (hs_ctrl5 >> 1) & 1,
                // Both locations have existed in Qualcomm HS-PHY revisions;
                // expose them independently instead of guessing in firmware.
                "hsphy-vbus-valid0" => (hs_qscratch >> 20) & 1,
                "hsphy-vbus-valid1" => (hs_qscratch >> 28) & 1,
                _ => u32::MAX,
            };
            if value != u32::MAX {
                return value;
            }
        }
        if !seen[0] {
            return 0;
        }

        match selector {
            "utmi-trdtim" => (gusb2 >> 10) & 0xf,
            "utmi-phyif" => (gusb2 >> 3) & 1,
            "utmi-susphy" => (gusb2 >> 6) & 1,
            "utmi-enblslpm" => (gusb2 >> 8) & 1,
            "utmi-freeclk" => (gusb2 >> 30) & 1,
            "utmi-parent" => (utmi_source >> 8) & 0x7,
            "utmi-div" => utmi_source & 0xff,
            "utmi-branch" => utmi_branch & 0xf,
            "utmi-gusb2-lo" => gusb2 & 0xf,
            "utmi-gusb2-hi" => (gusb2 >> 28) & 0xf,
            "utmi-link" => (dsts >> 18) & 0xf,
            "utmi-halt" => (dsts >> 22) & 1,
            "utmi-trdtim-stage1" => (gusb2_by_stage[1] >> 10) & 0xf,
            "utmi-trdtim-stage2" => (gusb2_by_stage[2] >> 10) & 0xf,
            "utmi-trdtim-stage3" => (gusb2_by_stage[3] >> 10) & 0xf,
            "utmi-trdtim-stage4" => (gusb2_by_stage[4] >> 10) & 0xf,
            "utmi-trdtim-stage5" => (gusb2_by_stage[5] >> 10) & 0xf,
            _ => 0,
        }
    }
}

/// Fill one page of the retained trace for the vendor control request. The
/// page order is oldest-to-newest within the valid window, so a host can read
/// a consistent bounded snapshot without knowing the physical RAM address.
/// A request for page zero returns the header even when the trace is empty;
/// malformed or out-of-range pages are rejected by returning `None`.
pub(super) unsafe fn fill_trace_control_response(
    response: &mut [u8],
    requested_length: usize,
    page: u16,
) -> Option<usize> {
    let requested_length = requested_length.min(response.len());
    if requested_length == 0 {
        return None;
    }
    unsafe {
        let magic = read_volatile(addr_of!(USB_TRACE).cast::<u32>());
        let version = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(1));
        if magic != USB_TRACE_MAGIC || version != USB_TRACE_VERSION {
            return None;
        }
        let head = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(2));
        let valid = (head as usize).min(USB_TRACE_CAPACITY);
        let page_start = (page as usize).checked_mul(TRACE_CONTROL_PAGE_ENTRIES)?;
        if page_start > valid {
            return None;
        }

        response[..requested_length].fill(0);
        let mut write_u32 = |offset: usize, value: u32| {
            if offset + 4 <= requested_length {
                response[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            }
        };
        write_u32(0, magic);
        write_u32(4, version);
        write_u32(8, head);
        write_u32(12, valid as u32);
        if requested_length <= TRACE_CONTROL_HEADER_BYTES {
            return Some(requested_length);
        }

        let available = valid.saturating_sub(page_start);
        let records = available
            .min(TRACE_CONTROL_PAGE_ENTRIES)
            .min((requested_length - TRACE_CONTROL_HEADER_BYTES) / TRACE_CONTROL_ENTRY_BYTES);
        let oldest = (head as usize).saturating_sub(valid);
        for index in 0..records {
            let slot = (oldest + page_start + index) % USB_TRACE_CAPACITY;
            let entry = read_volatile(
                addr_of!(USB_TRACE.entries)
                    .cast::<UsbTraceEntry>()
                    .add(slot),
            );
            let values = [
                entry.sequence,
                entry.event,
                entry.request,
                entry.value,
                entry.index,
                entry.length,
                entry.ep0_state,
                entry.status,
            ];
            let base = TRACE_CONTROL_HEADER_BYTES + index * TRACE_CONTROL_ENTRY_BYTES;
            for (word, value) in values.into_iter().enumerate() {
                response[base + word * 4..base + word * 4 + 4]
                    .copy_from_slice(&value.to_le_bytes());
            }
        }
        Some(TRACE_CONTROL_HEADER_BYTES + records * TRACE_CONTROL_ENTRY_BYTES)
    }
}

/// Dump the post-mortem USB trace after the controller has reached a safe
/// UART-visible stage. The hot path above never calls this or formats text.
pub fn dump_trace() {
    unsafe {
        let magic = read_volatile(addr_of!(USB_TRACE).cast::<u32>());
        let version = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(1));
        if magic != USB_TRACE_MAGIC || version != USB_TRACE_VERSION {
            super::super::uart::puts("usb trace: no retained record\n");
            return;
        }
        let head = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(2));
        let count = (head as usize).min(USB_TRACE_CAPACITY);
        let start = (head as usize).saturating_sub(count);
        super::super::uart::puts("usb trace begin\n");
        for offset in 0..count {
            let slot = (start + offset) % USB_TRACE_CAPACITY;
            let entry = read_volatile(
                addr_of!(USB_TRACE.entries)
                    .cast::<UsbTraceEntry>()
                    .add(slot),
            );
            super::super::uart::put_hex("usb trace event=", entry.event as u64);
            super::super::uart::put_hex(" request=", entry.request as u64);
            super::super::uart::put_hex(" value=", entry.value as u64);
            super::super::uart::put_hex(" index=", entry.index as u64);
            super::super::uart::put_hex(" length=", entry.length as u64);
            super::super::uart::put_hex(" state=", entry.ep0_state as u64);
            super::super::uart::put_hex(" status=", entry.status as u64);
        }
        super::super::uart::puts("usb trace end\n");
    }
}
