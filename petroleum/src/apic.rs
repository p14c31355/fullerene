//! Advanced Programmable Interrupt Controller (APIC) support
//!
//! Provides functions for configuring LAPIC and I/O APIC during UEFI boot.

use crate::{volatile_read, volatile_write};

/// I/O APIC register offsets
const IOAPIC_VER: u8 = 0x01;
const IOAPIC_REDTBL_START: u8 = 0x10;

/// I/O APIC Redirection Table Entry (RTE) structure
#[repr(C)]
#[derive(Clone, Copy)]
pub struct IoApicRedirectionEntry {
    pub lower: u32,
    pub upper: u32,
}

impl IoApicRedirectionEntry {
    /// Create a new RTE with specified parameters
    pub fn new(
        vector: u8,
        delivery_mode: u8,
        dest_mode: bool,
        polarity: bool,
        trigger: bool,
        mask: bool,
        dest: u8,
    ) -> Self {
        let lower = (vector as u32)
            | ((delivery_mode as u32) << 8)
            | ((dest_mode as u32) << 11)
            | ((polarity as u32) << 13)
            | ((trigger as u32) << 15)
            | ((mask as u32) << 16);

        let upper = (dest as u32) << 24;

        Self { lower, upper }
    }

    /// Set the vector
    pub fn set_vector(&mut self, vector: u8) {
        bit_field_set!(self.lower, 0xFF, 0, vector);
    }

    /// Set delivery mode
    pub fn set_delivery_mode(&mut self, mode: u8) {
        bit_field_set!(self.lower, 0x7, 8, mode);
    }

    /// Set destination mode (0 = physical, 1 = logical)
    pub fn set_dest_mode(&mut self, logical: bool) {
        set_bool_bit!(self.lower, 11, logical);
    }

    /// Set polarity (0 = high active, 1 = low active)
    pub fn set_polarity(&mut self, low_active: bool) {
        set_bool_bit!(self.lower, 13, low_active);
    }

    /// Set trigger mode (0 = edge, 1 = level)
    pub fn set_trigger_mode(&mut self, level: bool) {
        set_bool_bit!(self.lower, 15, level);
    }

    /// Set mask (0 = unmasked, 1 = masked)
    pub fn set_mask(&mut self, masked: bool) {
        set_bool_bit!(self.lower, 16, masked);
    }

    /// Set destination
    pub fn set_destination(&mut self, dest: u8) {
        bit_field_set!(self.upper, 0xFF, 24, dest);
    }
}

/// I/O APIC structure
#[derive(Clone, Copy)]
pub struct IoApic {
    base_addr: u64,
}

impl IoApic {
    /// Create new I/O APIC instance
    pub fn new(base_addr: u64) -> Self {
        Self { base_addr }
    }

    /// Read from I/O APIC register (volatile)
    unsafe fn read(&self, reg: u8) -> u32 {
        let reg_addr = self.base_addr as *mut u32;
        let value_addr = (self.base_addr + 0x10) as *mut u32;

        volatile_write!(reg_addr, reg as u32);
        volatile_read!(value_addr, u32)
    }

    /// Write to I/O APIC register (volatile)
    unsafe fn write(&self, reg: u8, value: u32) {
        let reg_addr = self.base_addr as *mut u32;
        let value_addr = (self.base_addr + 0x10) as *mut u32;

        volatile_write!(reg_addr, reg as u32);
        volatile_write!(value_addr, value);
    }

    /// Read redirection table entry
    pub fn read_rte(&self, index: u8) -> IoApicRedirectionEntry {
        unsafe {
            IoApicRedirectionEntry {
                lower: self.read(IOAPIC_REDTBL_START + index * 2),
                upper: self.read(IOAPIC_REDTBL_START + index * 2 + 1),
            }
        }
    }

    /// Write redirection table entry
    pub fn write_rte(&self, index: u8, entry: IoApicRedirectionEntry) {
        unsafe {
            self.write(IOAPIC_REDTBL_START + index * 2, entry.lower);
            self.write(IOAPIC_REDTBL_START + index * 2 + 1, entry.upper);
        }
    }

    /// Get version register
    pub fn get_version(&self) -> u32 {
        unsafe { self.read(IOAPIC_VER) }
    }

    /// Get maximum redirection entry (number of entries - 1)
    pub fn get_max_redirection_entry(&self) -> u8 {
        (unsafe { self.read(IOAPIC_VER) } >> 16) as u8
    }
}

/// Find I/O APIC base address (simplified for common systems)
/// In a full implementation, this would parse ACPI MADT
pub fn find_io_apic_base() -> u64 {
    // Default I/O APIC base for x86 systems
    0xFEC00000
}

/// Configure I/O APIC for legacy IRQs
pub fn configure_io_apic_for_legacy_irqs(io_apic: &mut IoApic, local_apic_id: u8) {
    // Configure keyboard (IRQ 1) -> vector 33
    let keyboard_rte =
        IoApicRedirectionEntry::new(33, 0, false, false, false, false, local_apic_id);
    io_apic.write_rte(1, keyboard_rte);

    // Configure mouse (IRQ 12) -> vector 44
    let mouse_rte = IoApicRedirectionEntry::new(44, 0, false, false, false, false, local_apic_id);
    io_apic.write_rte(12, mouse_rte);

    // Note: Other IRQs can be configured similarly as needed
}

/// Get local APIC ID from the LAPIC
pub unsafe fn get_local_apic_id(lapic_base: u64) -> u8 {
    let lapic_id_reg = (lapic_base + 0x20) as *const u32;
    (volatile_read!(lapic_id_reg, u32) >> 24) as u8
}

/// Initialize I/O APIC for legacy interrupts
pub fn init_io_apic(lapic_base: u64) {
    let local_apic_id = unsafe { get_local_apic_id(lapic_base) };
    let io_apic_base = find_io_apic_base();
    let mut io_apic = IoApic::new(io_apic_base);

    configure_io_apic_for_legacy_irqs(&mut io_apic, local_apic_id);
}
