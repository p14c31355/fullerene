//! xHCI register context — structured MMIO access layer.
//!
//! This module implements the "生アドレス・レジスタ操作の局所化" policy:
//! all raw reads/writes to xHCI MMIO registers are confined to
//! [`RegisterContext`], [`OperationalRegisters`], [`RuntimeRegisters`],
//! and [`DoorbellRegisters`].
//!
//! # Register layout overview (from xHCI spec §5.2–§5.5)
//!
//! ```text
//! MMIO BASE
//! ├── CapabilityRegisters    (read-only, CAPLENGTH byte)
//! ├── OperationalRegisters   (offset = CAPLENGTH)
//! ├── RuntimeRegisters       (offset = RT_OFF from HCCPARAMS1)
//! ├── DoorbellArray          (offset = DB_OFF from HCCPARAMS2)
//! └── ExtendedCapabilities   (optional)
//! ```

use alloc::vec::Vec;
use core::ptr;

// ============================================================================
//  Register Offsets
// ============================================================================

// ── Capability Registers (offset from MMIO base) ─────────────
pub const CAP_CAPLENGTH: usize = 0x00;
pub const CAP_HCSPARAMS1: usize = 0x04;
pub const CAP_HCSPARAMS2: usize = 0x08;
pub const CAP_HCSPARAMS3: usize = 0x0C;
pub const CAP_HCCPARAMS1: usize = 0x10;
pub const CAP_DBOFF: usize = 0x14;
pub const CAP_RTSOFF: usize = 0x18;

// ── Operational Registers (offset from CAPLENGTH) ────────────
pub const OP_USBCMD: usize = 0x00;
pub const OP_USBSTS: usize = 0x04;
pub const OP_PAGESIZE: usize = 0x08;
pub const OP_DNCTRL: usize = 0x14;
pub const OP_CRCR: usize = 0x18; // 64-bit (CRCR low = 0x18, high = 0x1C)
pub const OP_DCBAAP: usize = 0x30; // 64-bit (DCBAAP low = 0x30, high = 0x34)
pub const OP_CONFIG: usize = 0x38;

/// First port status register offset (each port is 16 bytes).
pub const OP_PORTSC_BASE: usize = 0x400;
pub const OP_PORTSC_STRIDE: usize = 0x10;

// ── Runtime Registers (offset from RTSOFF) ──────────────────
pub const RT_IMAN: usize = 0x00; // Interrupter Management
pub const RT_IMOD: usize = 0x04; // Interrupter Moderation
pub const RT_ERSTSZ: usize = 0x08; // Event Ring Segment Table Size
pub const RT_ERSTBA: usize = 0x10; // Event Ring Segment Table Base Address (64-bit)
pub const RT_ERDP: usize = 0x18; // Event Ring Dequeue Pointer (64-bit)
pub const RT_INTERRUPTER_STRIDE: usize = 0x20;

// ============================================================================
//  Register bit definitions
// ============================================================================

// ── USBCMD ───────────────────────────────────────────────────
pub const USBCMD_RS: u32 = 1 << 0; // Run/Stop
pub const USBCMD_HCRST: u32 = 1 << 1; // Host Controller Reset
pub const USBCMD_INTE: u32 = 1 << 2; // Interrupter Enable
pub const USBCMD_HSEE: u32 = 1 << 3; // Host System Error Enable

// ── USBSTS ───────────────────────────────────────────────────
pub const USBSTS_HCH: u32 = 1 << 0; // Host Controller Halted
pub const USBSTS_HSE: u32 = 1 << 2; // Host System Error
pub const USBSTS_EINT: u32 = 1 << 3; // Event Interrupt
pub const USBSTS_PCD: u32 = 1 << 4; // Port Change Detect
pub const USBSTS_SSS: u32 = 1 << 8; // Save State Status
pub const USBSTS_RSS: u32 = 1 << 9; // Restore State Status
pub const USBSTS_SRE: u32 = 1 << 10; // Save/Restore Error
pub const USBSTS_CNR: u32 = 1 << 11; // Controller Not Ready
pub const USBSTS_HCE: u32 = 1 << 12; // Host Controller Error

// ── PORTSC ───────────────────────────────────────────────────
pub const PORTSC_CCS: u32 = 1 << 0; // Current Connect Status
pub const PORTSC_PED: u32 = 1 << 1; // Port Enabled/Disabled
pub const PORTSC_OCA: u32 = 1 << 3; // Over-current Active
pub const PORTSC_PR: u32 = 1 << 4; // Port Reset
pub const PORTSC_PLS_MASK: u32 = 0xF << 5; // Port Link State
pub const PORTSC_PP: u32 = 1 << 9; // Port Power
pub const PORTSC_SPEED_MASK: u32 = 0xF << 10; // Port Speed
pub const PORTSC_PIC_MASK: u32 = 0x3 << 14; // Port Indicator Control
pub const PORTSC_LWS: u32 = 1 << 16; // Port Link State Write Strobe
pub const PORTSC_CSC: u32 = 1 << 17; // Connect Status Change
pub const PORTSC_PEC: u32 = 1 << 18; // Port Enabled/Disabled Change
pub const PORTSC_WRC: u32 = 1 << 19; // Warm Port Reset Change
pub const PORTSC_OCC: u32 = 1 << 20; // Over-current Change
pub const PORTSC_PRC: u32 = 1 << 21; // Port Reset Change
pub const PORTSC_PLC: u32 = 1 << 22; // Port Link State Change
pub const PORTSC_CEC: u32 = 1 << 23; // Config Error Change
pub const PORTSC_WPR: u32 = 1 << 31; // Warm Port Reset

/// All RW1C status bits (bits 17–23).
pub const PORTSC_RW1C_MASK: u32 =
    PORTSC_CSC | PORTSC_PEC | PORTSC_WRC | PORTSC_OCC | PORTSC_PRC | PORTSC_PLC | PORTSC_CEC;

// ── IMAN ─────────────────────────────────────────────────────
pub const IMAN_IP: u32 = 1 << 0; // Interrupt Pending
pub const IMAN_IE: u32 = 1 << 1; // Interrupt Enable

// ── CRCR ─────────────────────────────────────────────────────
pub const CRCR_RCS: u32 = 1 << 0; // Ring Cycle State
pub const CRCR_CS: u32 = 1 << 1; // Command Stop
pub const CRCR_CA: u32 = 1 << 2; // Command Abort
pub const CRCR_CRR: u32 = 1 << 3; // Command Ring Running

// ============================================================================
//  CapabilityRegisters
// ============================================================================

/// Read-only view of the xHCI capability registers (§5.2).
#[derive(Debug)]
pub struct CapabilityRegisters {
    pub caplength: u8,
    pub hci_version: u16,
    pub hcs_params1: u32,
    pub hcs_params2: u32,
    pub hcs_params3: u32,
    pub hcc_params1: u32,
    pub db_offset: u32,
    pub rt_offset: u32,
}

// ── Derived fields from HCSPARAMS1 ──────────────────────────
#[derive(Debug, Clone, Copy)]
pub struct HcsParams1 {
    pub max_slots: u32,
    pub max_interrupters: u32,
    pub n_ports: u32,
    pub ppc: bool, // Port Power Control
    pub csz: bool, // Context Size (0=32byte, 1=64byte)
    pub max_scratchpad_bufs: u32,
}

// ── Derived fields from HCCPARAMS1 ──────────────────────────
#[derive(Debug, Clone, Copy)]
pub struct HccParams1 {
    pub ac64: bool,        // 64-bit addressing capable
    pub bnc: bool,         // BW Negotiation Capable
    pub csz: bool,         // Context Size (different from CSZ in HCSPARAMS1)
    pub ppc: bool,         // PPC indicator
    pub pind: bool,        // Port Indicators
    pub lhrc: bool,        // Light HC Reset Capable
    pub ltc: bool,         // Latency Tolerance Messaging
    pub nss: bool,         // No Secondary SID
    pub psc: bool,         // Parse All Event Data
    pub ext_cap_ptr: u16,  // extended capabilities pointer
    pub max_psa_size: u32, // Maximum Primary Stream Array Size
}

// ============================================================================
//  OperationalRegisters — mutable, read/write via structured methods
// ============================================================================

/// Accessor for operational registers (offset = caplength).
///
/// All writes go through [`Self::write`], which also does a `clflush`.
pub struct OperationalRegisters {
    base: *mut u8,
}

// ── Register value structs (returned by reads) ───────────────

pub struct UsbCmd(pub u32);
impl UsbCmd {
    pub fn run_stop(&self) -> bool {
        self.0 & USBCMD_RS != 0
    }
    pub fn reset(&self) -> bool {
        self.0 & USBCMD_HCRST != 0
    }
    pub fn inte(&self) -> bool {
        self.0 & USBCMD_INTE != 0
    }
}

pub struct UsbSts(pub u32);
impl UsbSts {
    pub fn hchalted(&self) -> bool {
        self.0 & USBSTS_HCH != 0
    }
    pub fn hse(&self) -> bool {
        self.0 & USBSTS_HSE != 0
    }
    pub fn eint(&self) -> bool {
        self.0 & USBSTS_EINT != 0
    }
    pub fn pcd(&self) -> bool {
        self.0 & USBSTS_PCD != 0
    }
    pub fn cnr(&self) -> bool {
        self.0 & USBSTS_CNR != 0
    }
    pub fn hce(&self) -> bool {
        self.0 & USBSTS_HCE != 0
    }
}

pub struct PortSc(pub u32);
impl PortSc {
    pub fn ccs(&self) -> bool {
        self.0 & PORTSC_CCS != 0
    }
    pub fn ped(&self) -> bool {
        self.0 & PORTSC_PED != 0
    }
    pub fn pr(&self) -> bool {
        self.0 & PORTSC_PR != 0
    }
    pub fn pp(&self) -> bool {
        self.0 & PORTSC_PP != 0
    }
    pub fn pls(&self) -> u32 {
        (self.0 & PORTSC_PLS_MASK) >> 5
    }
    pub fn speed(&self) -> u32 {
        (self.0 & PORTSC_SPEED_MASK) >> 10
    }
    pub fn wpr(&self) -> bool {
        self.0 & PORTSC_WPR != 0
    }
    pub fn csc(&self) -> bool {
        self.0 & PORTSC_CSC != 0
    }
    pub fn pec(&self) -> bool {
        self.0 & PORTSC_PEC != 0
    }
}

// ============================================================================
//  RuntimeRegisters
// ============================================================================

/// Accessor for runtime registers (offset = RTSOFF).
pub struct RuntimeRegisters {
    base: *mut u8,
}

// ============================================================================
//  DoorbellRegisters
// ============================================================================

/// Accessor for the doorbell array (offset = DBOFF).
pub struct DoorbellRegisters {
    base: *mut u8,
}

// ============================================================================
//  RegisterContext — top-level container for all register accessors
// ============================================================================

/// Owner of the MMIO region + all register accessors.
///
/// This is the **only** place that does raw `read_volatile` /
/// `write_volatile` to xHCI MMIO.
pub struct RegisterContext {
    /// Raw MMIO base virtual address.
    pub mmio_base: *mut u8,
    /// Pre-parsed capability values.
    pub cap: CapabilityRegisters,
    /// Mutable operational registers.
    pub op: OperationalRegisters,
    /// Mutable runtime registers (offset = RTSOFF).
    pub runtime: RuntimeRegisters,
    /// Mutable doorbell array (offset = DBOFF).
    pub doorbell: DoorbellRegisters,
}

// ══════════════════════════════════════════════════════════════
//  Implementation
// ══════════════════════════════════════════════════════════════

impl CapabilityRegisters {
    /// Read all capability registers from the capability region.
    pub unsafe fn read(mmio: *mut u8) -> Self {
        let caplength = ptr::read_volatile(mmio as *const u8);
        let hci_version = ptr::read_volatile(mmio.add(0x02) as *const u16);
        Self {
            caplength,
            hci_version,
            hcs_params1: ptr::read_volatile(mmio.add(CAP_HCSPARAMS1) as *const u32),
            hcs_params2: ptr::read_volatile(mmio.add(CAP_HCSPARAMS2) as *const u32),
            hcs_params3: ptr::read_volatile(mmio.add(CAP_HCSPARAMS3) as *const u32),
            hcc_params1: ptr::read_volatile(mmio.add(CAP_HCCPARAMS1) as *const u32),
            db_offset: ptr::read_volatile(mmio.add(CAP_DBOFF) as *const u32) & 0xFFFF_FFFC,
            rt_offset: ptr::read_volatile(mmio.add(CAP_RTSOFF) as *const u32) & 0xFFFF_FFFC,
        }
    }

    pub fn hcs_params1(&self) -> HcsParams1 {
        HcsParams1 {
            max_slots: self.hcs_params1 & 0xFF,
            max_interrupters: (self.hcs_params1 >> 8) & 0x7FF,
            n_ports: (self.hcs_params1 >> 24) & 0xFF,
            ppc: (self.hcc_params1 >> 3) & 1 != 0,
            csz: (self.hcc_params1 >> 2) & 1 != 0,
            max_scratchpad_bufs: (self.hcs_params2 >> 27) & 0x1F
                | ((self.hcs_params2 >> 21) & 0x1F) << 5,
        }
    }

    pub fn hcc_params1(&self) -> HccParams1 {
        let raw = self.hcc_params1;
        HccParams1 {
            ac64: raw & 1 != 0,
            bnc: (raw >> 1) & 1 != 0,
            csz: (raw >> 2) & 1 != 0,
            ppc: (raw >> 3) & 1 != 0,
            pind: (raw >> 4) & 1 != 0,
            lhrc: (raw >> 5) & 1 != 0,
            ltc: (raw >> 6) & 1 != 0,
            nss: (raw >> 7) & 1 != 0,
            psc: (raw >> 9) & 1 != 0,
            ext_cap_ptr: ((raw >> 16) & 0xFFFF) as u16,
            max_psa_size: (raw >> 12) & 0xF,
        }
    }
}

impl OperationalRegisters {
    /// Create from the operational base (mmio + caplength).
    pub unsafe fn new(op_base: *mut u8) -> Self {
        Self { base: op_base }
    }

    // ── helpers ───────────────────────────────────────────────

    /// Flush the cache line for a given offset.
    fn clflush_offset(addr: *const u8) {
        unsafe { core::arch::asm!("clflush [{}]", in(reg) addr, options(nostack, preserves_flags)) }
    }

    /// Read a 32-bit MMIO register.
    ///
    /// clflush before read works around UEFI firmware that maps xHCI MMIO
    /// as WB (Write-Back) instead of UC (Uncacheable).  On WB mappings,
    /// read_volatile can return stale cached data.
    pub fn read(&self, offset: usize) -> u32 {
        let ptr = unsafe { self.base.add(offset) as *const u32 };
        Self::clflush_offset(ptr as *const u8);
        unsafe { ptr::read_volatile(ptr) }
    }

    pub fn write(&self, offset: usize, val: u32) {
        let ptr = unsafe { self.base.add(offset) as *mut u32 };
        unsafe { ptr::write_volatile(ptr, val) };
        Self::clflush_offset(ptr as *const u8);
    }

    pub fn read64(&self, low_off: usize) -> u64 {
        let lo = self.read(low_off);
        let hi = self.read(low_off + 4);
        (lo as u64) | ((hi as u64) << 32)
    }

    pub fn write64(&self, low_off: usize, val: u64) {
        self.write(low_off, val as u32);
        self.write(low_off + 4, (val >> 32) as u32);
    }

    // ── USBCMD ────────────────────────────────────────────────
    pub fn usbcmd(&self) -> UsbCmd {
        UsbCmd(self.read(OP_USBCMD))
    }
    pub fn set_usbcmd(&self, val: u32) {
        self.write(OP_USBCMD, val);
    }
    pub fn set_usbcmd_bits(&self, bits: u32) {
        self.set_usbcmd(self.read(OP_USBCMD) | bits);
    }
    pub fn clear_usbcmd_bits(&self, bits: u32) {
        self.set_usbcmd(self.read(OP_USBCMD) & !bits);
    }

    // ── USBSTS ────────────────────────────────────────────────
    pub fn usbsts(&self) -> UsbSts {
        UsbSts(self.read(OP_USBSTS))
    }
    /// Write-back RW1C bits (write '1' to clear).
    pub fn clear_usbsts_bits(&self, bits: u32) {
        self.write(OP_USBSTS, bits);
    }

    // ── CRCR ──────────────────────────────────────────────────
    pub fn crcr(&self) -> u64 {
        self.read64(OP_CRCR)
    }
    pub fn set_crcr(&self, val: u64) {
        self.write64(OP_CRCR, val);
    }

    // ── DCBAAP ────────────────────────────────────────────────
    pub fn dcbaap(&self) -> u64 {
        self.read64(OP_DCBAAP)
    }
    pub fn set_dcbaap(&self, val: u64) {
        self.write64(OP_DCBAAP, val);
    }

    // ── CONFIG ────────────────────────────────────────────────
    pub fn config(&self) -> u32 {
        self.read(OP_CONFIG)
    }
    pub fn set_config(&self, val: u32) {
        self.write(OP_CONFIG, val);
    }

    // ── PORTSC ────────────────────────────────────────────────
    pub fn portsc(&self, port: u32) -> PortSc {
        PortSc(self.read(OP_PORTSC_BASE + port as usize * OP_PORTSC_STRIDE))
    }
    pub fn write_portsc(&self, port: u32, val: u32) {
        self.write(OP_PORTSC_BASE + port as usize * OP_PORTSC_STRIDE, val);
    }
    /// Update PORTSC while preserving RW1C bits (write '0' to preserve them).
    pub fn update_portsc(&self, port: u32, set: u32, clear: u32) {
        let cur = self.read(OP_PORTSC_BASE + port as usize * OP_PORTSC_STRIDE);
        let val = ((cur & !clear) | set) & !PORTSC_RW1C_MASK;
        self.write(OP_PORTSC_BASE + port as usize * OP_PORTSC_STRIDE, val);
    }
}

impl RuntimeRegisters {
    pub unsafe fn new(rt_base: *mut u8) -> Self {
        Self { base: rt_base }
    }

    pub fn read(&self, offset: usize) -> u32 {
        let ptr = unsafe { self.base.add(offset) as *const u32 };
        OperationalRegisters::clflush_offset(ptr as *const u8);
        unsafe { ptr::read_volatile(ptr) }
    }

    pub fn write(&self, offset: usize, val: u32) {
        let ptr = unsafe { self.base.add(offset) as *mut u32 };
        unsafe { ptr::write_volatile(ptr, val) };
        OperationalRegisters::clflush_offset(ptr as *const u8);
    }

    pub fn read64(&self, low_off: usize) -> u64 {
        let lo = self.read(low_off);
        let hi = self.read(low_off + 4);
        (lo as u64) | ((hi as u64) << 32)
    }

    pub fn write64(&self, low_off: usize, val: u64) {
        self.write(low_off, val as u32);
        self.write(low_off + 4, (val >> 32) as u32);
    }

    // ── Interrupter 0 ─────────────────────────────────────────
    pub fn iman(&self) -> u32 {
        self.read(RT_IMAN)
    }
    pub fn set_iman(&self, val: u32) {
        self.write(RT_IMAN, val);
    }

    // ── Event Ring Segment Table ──────────────────────────────
    pub fn erstsz(&self) -> u32 {
        self.read(RT_ERSTSZ)
    }
    pub fn set_erstsz(&self, val: u32) {
        self.write(RT_ERSTSZ, val);
    }
    pub fn erstba(&self) -> u64 {
        self.read64(RT_ERSTBA)
    }
    pub fn set_erstba(&self, val: u64) {
        self.write64(RT_ERSTBA, val);
    }

    // ── Event Ring Dequeue Pointer ────────────────────────────
    pub fn erdp(&self) -> u64 {
        self.read64(RT_ERDP)
    }
    pub fn set_erdp(&self, val: u64) {
        self.write64(RT_ERDP, val);
    }
}

impl DoorbellRegisters {
    pub unsafe fn new(db_base: *mut u8) -> Self {
        Self { base: db_base }
    }

    pub fn ring(&self, slot: u32, stream: u32) {
        let off = slot as usize * 4; // each doorbell is 4 bytes per xHCI spec §5.6
        let val = (stream & 0xFF) | ((stream >> 8) & 0xFF) << 16;
        let ptr = unsafe { self.base.add(off) as *mut u32 };
        unsafe {
            ptr::write_volatile(ptr, val);
        }
        OperationalRegisters::clflush_offset(ptr as *const u8);
    }
}

impl RegisterContext {
    /// Create RegisterContext from the MMIO base address.
    pub unsafe fn new(mmio_base: *mut u8) -> Self {
        let cap = CapabilityRegisters::read(mmio_base);
        let op_base = mmio_base.add(cap.caplength as usize);
        let rt_base = mmio_base.add(cap.rt_offset as usize);
        let db_base = mmio_base.add(cap.db_offset as usize);

        RegisterContext {
            mmio_base,
            cap,
            op: OperationalRegisters::new(op_base),
            runtime: RuntimeRegisters::new(rt_base),
            doorbell: DoorbellRegisters::new(db_base),
        }
    }
}

// ============================================================================
//  Legacy Handoff — USB Legacy Support Capability (cap ID = 1)
// ============================================================================

/// Attempt legacy handoff (BIOS → OS) for the xHCI controller.
///
/// Returns `Ok(true)` if the OS already owns the controller,
/// Walk all extended capabilities and log them.
/// Useful for diagnosing BIOS handoff, protocol routing, and other EC issues.
pub fn dump_extended_capabilities(mmio_base: *mut u8, ext_cap_ptr: u16) {
    let mut ec_off = ext_cap_ptr as usize;
    let mut iterations = 0;
    while ec_off != 0 && ec_off < 0x100000 {
        iterations += 1;
        if iterations > 64 {
            log::warn!("xHCI: EC list exceeded max iterations");
            break;
        }
        let ec_id = unsafe { ptr::read_volatile(mmio_base.add(ec_off * 4) as *const u8) };
        let ec_next = unsafe { ptr::read_volatile(mmio_base.add(ec_off * 4 + 1) as *const u8) };
        let ec_dw1 = unsafe { ptr::read_volatile(mmio_base.add(ec_off * 4 + 4) as *const u32) };
        log::info!(
            "xHCI EC: id={} next={} DWORD1=0x{:08X} (offset 0x{:04x})",
            ec_id, ec_next, ec_dw1, ec_off * 4
        );
        if ec_id == 1 {
            // USBLEGSUP (offset 0): BIOS_SEM=bit16, OS_SEM=bit24
            let legsup = unsafe { ptr::read_volatile(mmio_base.add(ec_off * 4) as *const u32) };
            // USBLEGCTLSTS (offset 4): SMI enables in bits [4:0] and [23:19]
            let legctl = unsafe { ptr::read_volatile(mmio_base.add(ec_off * 4 + 4) as *const u32) };
            log::info!(
                "  → USB Legacy Support: BIOS_SEM={} OS_SEM={} SMI_en=0x{:03x}",
                (legsup >> 16) & 1,
                (legsup >> 24) & 1,
                legctl & 0x1F,
            );
        } else if ec_id == 2 {
            let dw2 = unsafe { ptr::read_volatile(mmio_base.add(ec_off * 4 + 8) as *const u32) };
            let port_offset = (dw2 & 0xFF) as u32;
            let port_count  = ((dw2 >> 8) & 0xFF) as u32;
            let major_rev   = unsafe {
                ptr::read_volatile(mmio_base.add(ec_off * 4) as *const u32) >> 24
            };
            log::info!(
                "  → Supported Protocol: ports {}-{} rev={}.0 {}",
                port_offset, port_offset + port_count - 1, major_rev,
                if major_rev >= 3 { "USB 3.x" } else { "USB 2.0" }
            );
        }
        if ec_next == 0 { break; }
        ec_off += ec_next as usize;
    }
}

/// Parse the Supported Protocol capability (ECID = 2) for each port.
///
/// Returns a bitmask per 32-port group: bit N set means port N is USB 3.0.
/// Ports not covered by any Supported Protocol entry default to USB 3.0 (bit=1).
/// The caller is responsible for allocating enough words: `(n_ports + 31) / 32`.
pub fn parse_port_protocols(mmio_base: *mut u8, ext_cap_ptr: u16, n_ports: u32) -> alloc::vec::Vec<u32> {
    let n_words = ((n_ports + 31) / 32).max(1) as usize;
    let mut bitmap = alloc::vec![0xFFFFFFFFu32; n_words];
    let mut ec_off = ext_cap_ptr as usize;
    let mut iterations = 0;

    while ec_off != 0 && ec_off < 0x100000 {
        iterations += 1;
        if iterations > 64 {
            log::warn!("xHCI: parse_port_protocols exceeded max iterations");
            break;
        }

        let ec_id = unsafe { ptr::read_volatile(mmio_base.add(ec_off * 4) as *const u8) };
        if ec_id == 2 {
            // Supported Protocol capability — DWORD2 is at offset 8
            let dw2 = unsafe { ptr::read_volatile(mmio_base.add(ec_off * 4 + 8) as *const u32) };
            let port_offset = (dw2 & 0xFF) as u32;        // 1-based
            if port_offset == 0 {
                continue;
            }
            let port_count  = ((dw2 >> 8) & 0xFF) as u32;
            let major_rev   = unsafe {
                ptr::read_volatile(mmio_base.add(ec_off * 4) as *const u32) >> 24
            };

            let is_usb3 = major_rev >= 3;
            log::info!(
                "xHCI: protocol cap: ports {}-{} {}",
                port_offset,
                port_offset + port_count - 1,
                if is_usb3 { "USB 3.x" } else { "USB 2.0" }
            );

            for p in 0..port_count {
                let port_idx = port_offset + p - 1; // 0-based
                if port_idx < n_ports {
                    let word = (port_idx / 32) as usize;
                    let bit  = port_idx % 32;
                    if word < bitmap.len() {
                        if is_usb3 {
                            bitmap[word] |= 1 << bit;
                        } else {
                            bitmap[word] &= !(1 << bit);
                        }
                    }
                }
            }
        }

        let ec_next = unsafe { ptr::read_volatile(mmio_base.add(ec_off * 4 + 1) as *const u8) };
        if ec_next == 0 {
            break;
        }
        ec_off += ec_next as usize;
    }

    bitmap
}

pub fn try_legacy_handoff(mmio_base: *mut u8, ext_cap_ptr: u16) -> Result<bool, &'static str> {
    let mut ec_off = ext_cap_ptr as usize;
    let mut iterations = 0;
    while ec_off != 0 && ec_off < 0x100000 {
        iterations += 1;
        if iterations > 64 {
            log::warn!("xHCI: try_legacy_handoff exceeded max iterations, possible circular list");
            return Err("circular capability list");
        }
        let ec_id = unsafe { ptr::read_volatile(mmio_base.add(ec_off * 4) as *const u8) };
        if ec_id == 1 {
            let cap_base = ec_off * 4; // byte offset of this capability

            // ── USBLEGSUP (offset 0): semaphore register ──
            //   bit 16 = HC BIOS Owned Semaphore
            //   bit 24 = HC OS Owned Semaphore
            let legsup = unsafe { ptr::read_volatile(mmio_base.add(cap_base) as *const u32) };
            let bios_sem = (legsup >> 16) & 1;
            let os_sem   = (legsup >> 24) & 1;
            log::info!(
                "USB Legacy Support: USBLEGSUP=0x{:08X} BIOS_SEM={} OS_SEM={}",
                legsup, bios_sem, os_sem
            );

            if bios_sem == 0 {
                log::info!("xHCI: OS already owns controller");
                // Even when OS already owns, clear SMI enables in USBLEGCTLSTS (offset 4)
                // bits [4:0] = SMI enables, bits [23:19] = additional SMI enables
                let legctl = unsafe { ptr::read_volatile(mmio_base.add(cap_base + 4) as *const u32) };
                let cleared = legctl & !0x00F8001F;
                unsafe {
                    ptr::write_volatile(mmio_base.add(cap_base + 4) as *mut u32, cleared);
                }
                return Ok(true);
            }

            log::info!("xHCI: BIOS owns controller — requesting handoff");
            // Request ownership: set OS_SEM bit (bit 24 of USBLEGSUP)
            let req = legsup | (1 << 24);
            unsafe {
                ptr::write_volatile(mmio_base.add(cap_base) as *mut u32, req);
            }

            // Wait for BIOS to clear BIOS_SEM (bit 16 of USBLEGSUP)
            let mut bios_cleared = false;
            for _ in 0..5_000_000 {
                let cur = unsafe { ptr::read_volatile(mmio_base.add(cap_base) as *const u32) };
                if (cur & (1 << 16)) == 0 {
                    bios_cleared = true;
                    break;
                }
                core::hint::spin_loop();
            }
            if !bios_cleared {
                log::info!("xHCI: legacy handoff timed out");
                return Err("legacy handoff timed out");
            }

            // Disable SMI enables in USBLEGCTLSTS (offset 4)
            // bits [4:0] and [23:19] control SMI generation on USB events
            let legctl = unsafe { ptr::read_volatile(mmio_base.add(cap_base + 4) as *const u32) };
            let final_ctl = legctl & !0x00F8001F;
            unsafe {
                ptr::write_volatile(mmio_base.add(cap_base + 4) as *mut u32, final_ctl);
            }

            let final_legsup =
                unsafe { ptr::read_volatile(mmio_base.add(cap_base) as *const u32) };
            log::info!("xHCI: legacy handoff done, USBLEGSUP=0x{:08X}", final_legsup);
            return Ok(false);
        }
        let ec_next = unsafe { ptr::read_volatile(mmio_base.add(ec_off * 4 + 1) as *const u8) };
        if ec_next == 0 {
            break;
        }
        ec_off += ec_next as usize;
    }
    Ok(true)
}

// ============================================================================
//  Port mappings
// ============================================================================

/// Convert xHCI port speed to generic USB speed.
///
/// xHCI PORTSC bits [13:10] encoding (xHCI 1.2 §5.4.8):
///   1 → Full (12 Mbps)
///   2 → Low  (1.5 Mbps)
///   3 → High (480 Mbps)
///   4 → SuperSpeed (5 Gbps, USB 3.0/3.1 Gen1)
///   5 → SuperSpeedPlus (10 Gbps, USB 3.1 Gen2)
pub fn port_speed_to_usb(speed: u32) -> crate::usb::UsbSpeed {
    match speed {
        3 => crate::usb::UsbSpeed::High,
        2 => crate::usb::UsbSpeed::Low,
        1 => crate::usb::UsbSpeed::Full,
        4 | 5 => {
            log::info!("xHCI: SuperSpeed device detected (speed={})", speed);
            crate::usb::UsbSpeed::SuperSpeed
        }
        _ => {
            log::warn!("xHCI: unknown port speed {}, defaulting to High", speed);
            crate::usb::UsbSpeed::High
        }
    }
}

// ============================================================================
//  Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usbsts_bitfields() {
        let sts = UsbSts(USBSTS_HCH | USBSTS_HSE);
        assert!(sts.hchalted());
        assert!(sts.hse());
        assert!(!sts.eint());
    }

    #[test]
    fn test_portsc_bitfields() {
        let ps = PortSc(PORTSC_CCS | PORTSC_PP | 5 << 5); // CCS + PP, PLS=5
        assert!(ps.ccs());
        assert!(ps.pp());
        assert_eq!(ps.pls(), 5);
        assert!(!ps.ped());
    }

    #[test]
    fn test_hcs_params1_parsing() {
        let cap = CapabilityRegisters {
            caplength: 0x20,
            hci_version: 0x0100,
            hcs_params1: 0x080000FF, // bits[31:24]=n_ports=8, bits[7:0]=max_slots=255
            hcs_params2: 0,
            hcs_params3: 0,
            hcc_params1: 0,
            db_offset: 0x1000,
            rt_offset: 0x2000,
        };
        let p = cap.hcs_params1();
        assert_eq!(p.max_slots, 255);
        assert_eq!(p.n_ports, 8);
        assert!(!p.ppc);
    }
}
