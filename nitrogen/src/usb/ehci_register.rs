//! EHCI register context — structured MMIO access layer.
//!
//! Confines all raw `read_volatile` / `write_volatile` for EHCI
//! operational registers to [`EhciOperationalRegisters`].
//!
//! # Register layout (EHCI spec §2.1)
//!
//! ```text
//! MMIO BASE
//! ├── Capability Registers  (read-only, offset 0x00–)
//! └── Operational Registers (offset = CAPLENGTH)
//!     ├── USBCMD         (0x00)
//!     ├── USBSTS         (0x04)
//!     ├── ASYNCLISTADDR  (0x18)
//!     └── PORTSC[0..N]   (0x44 + port*4)
//! ```

use core::ptr;

// ============================================================================
//  Register offsets
// ============================================================================

pub const OP_USBCMD: usize = 0x00;
pub const OP_USBSTS: usize = 0x04;
pub const OP_ASYNCLISTADDR: usize = 0x18;
pub const OP_PORTSC_BASE: usize = 0x44;

// ============================================================================
//  Register bit definitions
// ============================================================================

// ── USBCMD ───────────────────────────────────────────────────
pub const USBCMD_RS: u32 = 1 << 0;       // Run/Stop
pub const USBCMD_HCRESET: u32 = 1 << 1;  // Host Controller Reset
pub const USBCMD_ASSE: u32 = 1 << 5;     // Async Schedule Enable
pub const USBCMD_IAAD: u32 = 1 << 6;     // Interrupt on Async Advance Doorbell

// ── USBSTS ───────────────────────────────────────────────────
pub const USBSTS_HCH: u32 = 1 << 0;      // Host Controller Halted
pub const USBSTS_PCD: u32 = 1 << 2;      // Port Change Detect
pub const USBSTS_AAINT: u32 = 1 << 5;    // Async Advance Interrupt

// ── PORTSC ───────────────────────────────────────────────────
pub const PORTSC_CCS: u32 = 1 << 0;      // Current Connect Status
pub const PORTSC_PE: u32 = 1 << 2;       // Port Enabled
pub const PORTSC_RESET: u32 = 1 << 8;    // Port Reset

// ============================================================================
//  EhciOperationalRegisters
// ============================================================================

/// Accessor for EHCI operational registers.
///
/// All MMIO reads/writes go through this struct.
pub struct EhciOperationalRegisters {
    base: *mut u8,
}

impl EhciOperationalRegisters {
    /// Create from the operational base (mmio + caplength).
    pub unsafe fn new(op_base: *mut u8) -> Self {
        Self { base: op_base }
    }

    pub fn read(&self, offset: usize) -> u32 {
        let ptr = unsafe { self.base.add(offset) as *const u32 };
        unsafe { ptr::read_volatile(ptr) }
    }

    pub fn write(&self, offset: usize, val: u32) {
        let ptr = unsafe { self.base.add(offset) as *mut u32 };
        unsafe { ptr::write_volatile(ptr, val) };
    }

    // ── USBCMD ────────────────────────────────────────────────
    pub fn usbcmd(&self) -> u32 { self.read(OP_USBCMD) }
    pub fn set_usbcmd(&self, val: u32) { self.write(OP_USBCMD, val); }
    pub fn set_usbcmd_bits(&self, bits: u32) {
        let cur = self.read(OP_USBCMD);
        self.write(OP_USBCMD, cur | bits);
    }

    // ── USBSTS ────────────────────────────────────────────────
    pub fn usbsts(&self) -> u32 { self.read(OP_USBSTS) }
    pub fn write_usbsts(&self, val: u32) { self.write(OP_USBSTS, val); }

    // ── ASYNCLISTADDR ─────────────────────────────────────────
    pub fn set_async_list_addr(&self, val: u32) {
        self.write(OP_ASYNCLISTADDR, val);
    }

    // ── PORTSC ────────────────────────────────────────────────
    pub fn portsc(&self, port: u32) -> u32 {
        self.read(OP_PORTSC_BASE + port as usize * 4)
    }
    pub fn write_portsc(&self, port: u32, val: u32) {
        self.write(OP_PORTSC_BASE + port as usize * 4, val);
    }
}

// ============================================================================
//  EhciRegisterContext — top-level register container
// ============================================================================

/// All EHCI register accessors.
pub struct EhciRegisterContext {
    /// Raw MMIO base virtual address.
    pub mmio_base: *mut u8,
    /// CAPLENGTH register value.
    pub caplength: u8,
    /// Operational registers.
    pub op: EhciOperationalRegisters,
}

impl EhciRegisterContext {
    /// Create from MMIO base address.
    pub unsafe fn new(mmio_base: *mut u8) -> Self {
        let caplength = ptr::read_volatile(mmio_base as *const u8);
        let op_base = mmio_base.add(caplength as usize);
        Self {
            mmio_base,
            caplength,
            op: EhciOperationalRegisters::new(op_base),
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
    fn test_portsc_bits() {
        assert_eq!(PORTSC_CCS, 1);
        assert_eq!(PORTSC_PE, 4);
        assert_eq!(PORTSC_RESET, 256);
    }
}