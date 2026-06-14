//! xHCI Debug Capability (DbC) — USB debug output without a full USB host
//! controller driver.
//!
//! The xHCI Debug Capability creates a simple debug device on the USB bus
//! when enabled.  The development machine sees it as a USB debug class
//! device; the target machine sends debug output through OUT transfer ring.
//!
//! # Usage
//!
//! ```ignore
//! // 1. Find xHC on PCI, map BAR0 to virtual address
//! // 2. Call init(bar0_virt) once
//! // 3. Use dbc_write_byte / dbc_write_bytes for output
//! ```

use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

// ── xHCI Extended Capability IDs ────────────────────────────────────

/// Debug Capability ID in the xHCI extended capability list.
const XHCI_EXT_CAP_DBC: u32 = 10;

// ── DbC register offsets (from capability base) ─────────────────────

const DBC_DBCCMD: usize = 0x04; // Debug Command Register
const DBC_DBCST: usize = 0x08; // Debug Status Register
const DBC_DBCCP: usize = 0x0C; // Debug Capability Parameters
const DBC_DBCDI1: usize = 0x10; // Debug Device Descriptor Info 1
const DBC_DBCDI2: usize = 0x14; // Debug Device Descriptor Info 2

// Event Ring registers
const DBC_DBCERDP: usize = 0x28; // Event Ring Dequeue Pointer (low)
const DBC_DBCERDP_HI: usize = 0x2C; // Event Ring Dequeue Pointer (high)
const DBC_DBCERSTSZ: usize = 0x14; // Event Ring Segment Table Size
const DBC_DBCERSTBA: usize = 0x18; // Event Ring Segment Table Base Address (low)
const DBC_DBCERSTBA_HI: usize = 0x1C; // Event Ring Segment Table Base Address (high)

// OUT endpoint context registers (offset from EP context base, varies)
// For DbC, endpoint contexts are at a fixed offset from capability base.
// Endpoint OUT context: 32 bytes per endpoint context
//   BulkBulkOut: 0x00-0x1F
//   BulkBulkIn:  0x20-0x3F
// The EP context base is at DBC_DBCCMD + some offset that depends on DBC version.
// For DbC v1 (common), the EP context base is at offset 0x30.
const DBC_EPCTX_OUT: usize = 0x60; // EP OUT context (TR dequeue ptr etc.)

// ── DbC Command Register bits ───────────────────────────────────────

const DBCCMD_DCE: u32 = 1 << 0; // Debug Capability Enable

// ── DbC Status Register bits ────────────────────────────────────────

const DBCST_DCE: u32 = 1 << 0; // Debug Capability Enabled
const DBCST_DRC: u32 = 1 << 3; // Debug Ready for Communication

// ── TRB (Transfer Request Block) ────────────────────────────────────

/// A single Transfer Request Block — 16 bytes, 16-byte aligned.
#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct Trb {
    parameter: u64,
    status: u32,
    control: u32,
}

impl Trb {
    const fn zeroed() -> Self {
        Self {
            parameter: 0,
            status: 0,
            control: 0,
        }
    }

    fn set_normal(&mut self, data_buf_phys: u64, len: u32) {
        self.parameter = data_buf_phys;
        self.status = len; // TD Size = 0, Transfer Length = len
        self.control = (1 << 10) // Interrupt on Completion
            | (1u32 << 0); // TRB Type = Normal (1)
    }

    fn set_link(&mut self, ring_seg_phys: u64) {
        self.parameter = ring_seg_phys;
        self.control = (1u32 << 1) // Toggle Cycle (not needed for link)
            | (2u32 << 10) // Interrupt on Completion = 2 (ignore)
            | (6u32 << 0); // TRB Type = Link (6)
    }
}

// ── Ring buffer sizes ───────────────────────────────────────────────

/// Number of TRBs in the OUT transfer ring.
const OUT_RING_TRBS: usize = 32;
/// Number of TRBs in the Event Ring Segment Table entry (1 segment).
const EVENT_RING_TRBS: usize = 64;

// ── Event Ring Segment Table Entry ──────────────────────────────────

#[repr(C, align(64))]
struct ErstEntry {
    ring_segment_base: u64,  // physical address of event ring segment
    ring_segment_size: u16,  // number of TRBs in this segment
    _rsvd: [u16; 3],
    _rsvd2: u64,
}

// ── Endpoint Context (simplified) ───────────────────────────────────

/// Bulk OUT endpoint context (32 bytes per EP context, xHCI spec §6.2.3)
#[repr(C, align(32))]
struct EpContext {
    _rsvd0: [u32; 4],       // 0x00-0x0F: reserved
    tr_dequeue_ptr: u64,     // 0x10-0x17: TR Dequeue Pointer
    _rsvd1: u32,             // 0x18-0x1B
    _rsvd2: u32,             // 0x1C-0x1F: TR Dequeue Pointer (hi)
}

// ── Global state ────────────────────────────────────────────────────

/// Physical address offset (higher-half base → physical).
/// Set once before `init()`.
static PHYS_OFFSET: spin::Mutex<u64> = spin::Mutex::new(0);

/// Whether DbC has been successfully initialised.
static DBC_READY: AtomicBool = AtomicBool::new(false);

/// Pointer to xHC MMIO BAR0 base (virtual address).
static MMIO_BASE: spin::Mutex<usize> = spin::Mutex::new(0);

/// Pointer to DbC capability registers (virtual address).
static DBC_CAP_BASE: spin::Mutex<usize> = spin::Mutex::new(0);

/// Ring buffer index — next TRB to write (monotonic, wraps via Link TRB).
static OUT_RING_IDX: spin::Mutex<usize> = spin::Mutex::new(0);

// ── Statically allocated ring buffers ───────────────────────────────
//
// These live in .bss and are page-aligned, so the physical address is
// simply `&buf as *const _ as u64 - PHYS_OFFSET`.
//
// The OUT ring has OUT_RING_TRBS + 1 TRBs (the last is a Link TRB).
// Data buffers: one 4 KiB page for bulk OUT data.

#[repr(C, align(4096))]
struct OutRingPage {
    trbs: [Trb; OUT_RING_TRBS + 1],
}

#[repr(C, align(4096))]
struct DataBufferPage {
    data: [u8; 4096],
}

#[repr(C, align(4096))]
struct EventRingPage {
    trbs: [Trb; EVENT_RING_TRBS],
}

#[repr(C, align(4096))]
struct ErstPage {
    ent: ErstEntry,
}

static mut OUT_RING: OutRingPage = OutRingPage {
    trbs: [Trb::zeroed(); OUT_RING_TRBS + 1],
};

static mut DATA_BUF: DataBufferPage = DataBufferPage { data: [0u8; 4096] };

static mut EVENT_RING: EventRingPage = EventRingPage {
    trbs: [Trb::zeroed(); EVENT_RING_TRBS],
};

static mut ERST: ErstPage = ErstPage {
    ent: ErstEntry {
        ring_segment_base: 0,
        ring_segment_size: 0,
        _rsvd: [0u16; 3],
        _rsvd2: 0,
    },
};

// ── Helper: virtual → physical ─────────────────────────────────────

fn virt_to_phys(va: u64) -> u64 {
    let offset = *PHYS_OFFSET.lock();
    if va >= offset {
        va - offset
    } else {
        // Identity-mapped
        va
    }
}

// ── Helper: MMIO register I/O ──────────────────────────────────────

unsafe fn mmio_read32(base: usize, offset: usize) -> u32 {
    let addr = (base + offset) as *const u32;
    ptr::read_volatile(addr)
}

unsafe fn mmio_write32(base: usize, offset: usize, val: u32) {
    let addr = (base + offset) as *mut u32;
    ptr::write_volatile(addr, val);
}

unsafe fn mmio_write64(base: usize, offset: usize, val: u64) {
    let addr = (base + offset) as *mut u64;
    ptr::write_volatile(addr, val);
}

// ── Public API ──────────────────────────────────────────────────────

/// Set the higher-half physical memory offset.
///
/// Must be called once before [`init`].
/// For Fullerene, this is `0xFFFF_8000_0000_0000`.
pub fn set_physical_offset(offset: u64) {
    *PHYS_OFFSET.lock() = offset;
}

/// Walk the xHCI extended capability list starting from `hccparams1`
/// and return the MMIO offset of the Debug Capability, or 0.
///
/// `bar0` is the virtual address of the xHC MMIO BAR0.
/// `hccparams1` is the value read from the xHC HCCPARAMS1 register
/// (offset 0x10 from BAR0), bits [15:0] give the xECP offset.
///
/// Returns the byte offset from BAR0 to the DbC capability base,
/// or 0 if not found.
pub fn find_dbc_capability(bar0: usize, hccparams1: u32) -> usize {
    let xecp = (hccparams1 >> 16) as usize; // bits [31:16] if xHCI 1.1+
    // Actually xHCI 1.0/1.1: HCCPARAMS1 bits [15:0] = xECP
    // Let's just read it correctly:
    let mut offset = (hccparams1 & 0xFFFF) as usize;

    if offset == 0 {
        return 0;
    }

    // Extended Capabilities Pointer is a DWORD offset from BAR0 base.
    // Each entry: DWORD 0 = [CAP_ID:7:0][NEXT:15:8][...]
    let mut o = offset << 2; // convert DWORD offset to byte offset

    for _ in 0..64 {
        // Sanity guard
        if o == 0 || o > 0x1000 {
            break;
        }

        let cap = unsafe { mmio_read32(bar0, o) };
        let cap_id = cap & 0xFF;
        let next = ((cap >> 8) & 0xFF) as usize;

        if cap_id == XHCI_EXT_CAP_DBC {
            return o;
        }

        if next == 0 {
            break;
        }
        o = next << 2; // DWORD offset → byte offset
    }

    0
}

/// Initialize the xHCI Debug Capability.
///
/// `bar0` is the virtual address of the xHC MMIO BAR0.
/// `dbc_offset` is the byte offset of the DbC capability within BAR0,
/// obtained from [`find_dbc_capability`].
///
/// # Safety
///
/// `bar0` must be a valid, mapped MMIO virtual address covering the
/// xHC register space and the DbC capability area.
pub unsafe fn init(bar0: usize, dbc_offset: usize) -> bool {
    if dbc_offset == 0 {
        return false;
    }

    *MMIO_BASE.lock() = bar0;
    let dbc_base = bar0 + dbc_offset;
    *DBC_CAP_BASE.lock() = dbc_base;

    // ── Check DBCCP for supported features ──────────────────────
    let dbccp = unsafe { mmio_read32(dbc_base, DBC_DBCCP) };
    let _max_burst = dbccp & 0xFF;
    // bits[7:0]   = MaxBurstSize
    // bits[15:8]  = Protocol
    // ...

    // ── Stop DbC if running (clear DCE) ─────────────────────────
    unsafe {
        let dbccmd = mmio_read32(dbc_base, DBC_DBCCMD);
        mmio_write32(dbc_base, DBC_DBCCMD, dbccmd & !DBCCMD_DCE);
    }

    // Wait for DCE to clear
    for _ in 0..10000 {
        let dbcst = unsafe { mmio_read32(dbc_base, DBC_DBCST) };
        if (dbcst & DBCST_DCE) == 0 {
            break;
        }
        core::hint::spin_loop();
    }

    // ── Set up Event Ring Segment Table ─────────────────────────
    let ev_ring_phys = virt_to_phys(unsafe { &raw const EVENT_RING } as u64);
    let erst_phys = virt_to_phys(unsafe { &raw const ERST } as u64);

    unsafe {
        (&raw mut ERST).write(ErstPage {
            ent: ErstEntry {
                ring_segment_base: ev_ring_phys,
                ring_segment_size: EVENT_RING_TRBS as u16,
                _rsvd: [0u16; 3],
                _rsvd2: 0,
            },
        });
    }

    // Program ERSTSZ and ERSTBA
    unsafe {
        mmio_write32(dbc_base, DBC_DBCERSTSZ, 1); // 1 segment
        mmio_write64(dbc_base, DBC_DBCERSTBA, erst_phys);
    }

    // Set Event Ring Dequeue Pointer to start of event ring
    unsafe {
        mmio_write64(dbc_base, DBC_DBCERDP, ev_ring_phys);
    }

    // ── Set up OUT transfer ring ─────────────────────────────────
    let out_ring_phys = virt_to_phys(unsafe { &raw const OUT_RING } as u64);
    let out_trb_phys = out_ring_phys;

    // Program Bulk OUT endpoint context: TR Dequeue Pointer
    let epctx_base = dbc_base + DBC_EPCTX_OUT;
    unsafe {
        // EP context: TR Dequeue Pointer at offset 0x10
        let dp_addr = (epctx_base + 0x10) as *mut u64;
        ptr::write_volatile(dp_addr, out_trb_phys | 1); // DCS = 1 (cycle state)
        // DCS bit 0 = 1 means producer cycle state starts at 1
    }

    // ── Initialize OUT ring with Link TRB at the end ─────────────
    unsafe {
        for i in 0..OUT_RING_TRBS {
            OUT_RING.trbs[i] = Trb::zeroed();
        }
        // Link TRB points back to the beginning
        OUT_RING.trbs[OUT_RING_TRBS].set_link(out_ring_phys);
        OUT_RING.trbs[OUT_RING_TRBS].control |= 1 << 5; // Toggle Cycle (TC)
    }

    // ── Enable DbC ──────────────────────────────────────────────
    unsafe {
        mmio_write32(dbc_base, DBC_DBCCMD, DBCCMD_DCE);
    }

    // Wait for DCE and DRC to go high
    for _ in 0..100_000 {
        let dbcst = unsafe { mmio_read32(dbc_base, DBC_DBCST) };
        if (dbcst & DBCST_DCE) != 0 && (dbcst & DBCST_DRC) != 0 {
            DBC_READY.store(true, Ordering::Release);
            return true;
        }
        core::hint::spin_loop();
    }

    // DRC didn't go high — USB cable may not be connected yet.
    // Still mark as ready; writes will be queued and sent when
    // the host connects.
    let dbcst = unsafe { mmio_read32(dbc_base, DBC_DBCST) };
    if (dbcst & DBCST_DCE) != 0 {
        DBC_READY.store(true, Ordering::Release);
        return true;
    }

    false
}

/// Returns `true` if DbC has been initialized and is ready.
pub fn is_ready() -> bool {
    DBC_READY.load(Ordering::Acquire)
}

/// Write a single byte via the DbC OUT endpoint.
///
/// Data is buffered in a 4 KiB internal ring buffer and flushed
/// when full or when a newline (`\n`) is encountered.
pub fn dbc_write_byte(byte: u8) {
    if !is_ready() {
        return;
    }

    let mut idx_guard = OUT_RING_IDX.lock();
    let idx = *idx_guard;

    // Data buffer: simple ring of 4 KiB
    let buf_offset = idx % 4096;
    unsafe {
        let buf_ptr = &raw mut DATA_BUF.data as *mut u8;
        ptr::write_volatile(buf_ptr.add(buf_offset), byte);
    }

    // Flush on newline or when buffer is nearly full
    if byte == b'\n' || buf_offset >= 4095 {
        let bytes_to_send = if byte == b'\n' {
            (idx % 4096) + 1
        } else {
            4096
        };

        // Get the start of this chunk
        let chunk_start = idx.saturating_sub(bytes_to_send - 1);

        // Advance the ring index
        *idx_guard = idx + 1;
        drop(idx_guard);

        dbc_flush_chunk(chunk_start % 4096, bytes_to_send);
    } else {
        *idx_guard = idx + 1;
    }
}

/// Write raw bytes via DbC.  Flushes at the end.
pub fn dbc_write_bytes(bytes: &[u8]) {
    for &b in bytes {
        dbc_write_byte(b);
    }
}

/// Write a formatted string via DbC.
pub fn dbc_write_str(s: &str) {
    dbc_write_bytes(s.as_bytes());
}

// ── PCI device discovery helper ────────────────────────────────────

/// Search for an xHCI controller in the PCI device list.
///
/// Returns `(bus, device, function, bar0_physical_address)` for the first
/// xHC found, or `None`.
pub fn find_xhc_device(devices: &[crate::pci::PciDevice]) -> Option<(u8, u8, u8, u64)> {
    for dev in devices.iter() {
        let header_type =
            crate::pci::PciConfigSpace::read_config_byte(dev.bus, dev.device, dev.function, 0x0E);
        // xHCI controllers use header type 0x00 (non-bridge)
        if header_type & 0x7F != 0x00 {
            continue;
        }

        let class =
            crate::pci::PciConfigSpace::read_config_byte(dev.bus, dev.device, dev.function, 0x0B);
        let subclass =
            crate::pci::PciConfigSpace::read_config_byte(dev.bus, dev.device, dev.function, 0x0A);
        let prog_if =
            crate::pci::PciConfigSpace::read_config_byte(dev.bus, dev.device, dev.function, 0x09);

        // xHCI: Class 0x0C (Serial Bus), Subclass 0x03 (USB), ProgIF 0x30
        if class == 0x0C && subclass == 0x03 && prog_if == 0x30 {
            if let Some(bar0) = dev.read_bar(0) {
                return Some((dev.bus, dev.device, dev.function, bar0));
            }
        }
    }
    None
}

// ── Internal: flush a chunk of the data buffer to the OUT ring ──────

fn dbc_flush_chunk(buf_start: usize, len: usize) {
    let dbc_base = *DBC_CAP_BASE.lock();
    if dbc_base == 0 {
        return;
    }

    let actual_len = len.min(4096);

    // Get physical address of DATA_BUF
    let data_phys = virt_to_phys(unsafe { &raw const DATA_BUF } as u64);
    let chunk_phys = data_phys + buf_start as u64;

    // Find an available TRB in the OUT ring
    // For simplicity, we use a simple polling producer/consumer model.
    // Read the EP context to find the current dequeue pointer.
    let epctx_base = dbc_base + DBC_EPCTX_OUT;
    let dp_phys = unsafe {
        let dp_addr = (epctx_base + 0x10) as *const u64;
        ptr::read_volatile(dp_addr)
    };
    let dp_phys_clean = dp_phys & !0xF; // clear flags

    let out_ring_phys = virt_to_phys(unsafe { &raw const OUT_RING } as u64);

    // Find a free TRB: walk from producer (OUT_RING_IDX) to consumer (dp_phys_clean)
    let producer = *OUT_RING_IDX.lock() % OUT_RING_TRBS;

    // Place a Normal TRB
    unsafe {
        OUT_RING.trbs[producer].set_normal(chunk_phys, actual_len as u32);
    }

    // Ring the doorbell to notify xHC of new TRBs
    // For DbC, the doorbell mechanism is implicit — the xHC polls
    // the transfer ring continuously when DCE is set.
    // But we need to ensure the TRB cycle bit matches.
    //
    // Actually for DbC, we need to set the cycle bit on the TRB
    // to match the producer cycle state (PCS).
    // For simplicity, we set PCS = 1 (bit 0 of control).
    // The xHC will toggle it when consumed.
    let trb_ctrl = unsafe { ptr::read_volatile(&OUT_RING.trbs[producer].control) };
    unsafe {
        ptr::write_volatile(&mut OUT_RING.trbs[producer].control, trb_ctrl | 1);
    }

    // Advance producer index
    {
        let mut idx = OUT_RING_IDX.lock();
        *idx = (*idx + 1) % (OUT_RING_TRBS * 4096); // wrap at next index boundary
    }
}