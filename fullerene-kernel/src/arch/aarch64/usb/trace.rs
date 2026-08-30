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
        core::arch::asm!("dsb sy", options(nostack));
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
