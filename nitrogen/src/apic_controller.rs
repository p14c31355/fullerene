//! APIC Controller — unified Local APIC + I/O APIC + legacy PIC management
//!
//! This module provides a single `ApicController` struct that encapsulates
//! all unsafe MMIO volatile access, port I/O, and state management for the
//! x86-64 interrupt controller subsystem.
//!
//! # Design
//!
//! - Low‑level constants (`ApicOffsets`, `ApicFlags`) remain in `apic.rs`.
//! - `IoApicRedirectionEntry` remains in `ioapic.rs` (pure data).
//! - All register read/write, EOI, LVT programming, PIC disable, and
//!   I/O APIC routing live **inside** `ApicController` impl blocks.
//! - Callers (kernel, petroleum) only hold a `&mut ApicController` or
//!   share it behind a lock — they never need `unsafe` for APIC access.

use crate::apic::{ApicFlags, ApicOffsets};
use crate::ioapic::IoApicRedirectionEntry;
use core::ptr::{read_volatile, write_volatile};
use x86_64::instructions::port::Port;

// ── Legacy PIC constants (moved from pic.rs to keep PIC logic together) ──

const PIC_MASTER_COMMAND: u16 = 0x20;
const PIC_MASTER_DATA: u16 = 0x21;
const PIC_SLAVE_COMMAND: u16 = 0xA0;
const PIC_SLAVE_DATA: u16 = 0xA1;

const ICW1_INIT: u8 = 0x10;
const ICW4_8086: u8 = 0x01;

// ── I/O APIC register offsets ──

const IOAPIC_REG_WINDOW: u64 = 0x10;
const IOAPIC_VER: u8 = 0x01;
const IOAPIC_REDTBL_START: u8 = 0x10;

// ── ApicController ─────────────────────────────────────────────────────

/// Unified APIC controller encompassing Local APIC, I/O APIC, and legacy PIC.
///
/// After construction, all hardware interaction goes through method calls on
/// this struct. The caller is responsible for providing correct physical
/// base addresses (validated once at construction time).
#[repr(C)]
pub struct ApicController {
    /// Virtual base address of the Local APIC MMIO region.
    lapic_base: u64,

    /// Virtual base address of the I/O APIC MMIO region.
    ioapic_base: u64,

    /// Cached Local APIC ID (read from LAPIC register at construction).
    local_apic_id: u8,

    /// Cached I/O APIC version register.
    ioapic_version: u32,

    /// Cached maximum redirection entry index.
    max_redirection_entry: u8,
}

impl ApicController {
    // ── Construction ────────────────────────────────────────────────

    /// Create a new `ApicController`.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that `lapic_virt_base` and `ioapic_virt_base`
    /// are valid, page‑aligned virtual addresses pointing to the respective
    /// MMIO regions.  Typically these are higher‑half addresses computed from
    /// `PHYSICAL_MEMORY_OFFSET_BASE + physical_base`.
    ///
    /// This function performs volatile reads to cache the LAPIC ID and I/O APIC
    /// version — it must be called **after** the MMIO regions have been mapped.
    pub unsafe fn new(lapic_virt_base: u64, ioapic_virt_base: u64) -> Self {
        // Cache LAPIC ID
        let lapic_id_reg = (lapic_virt_base + ApicOffsets::ID as u64) as *const u32;
        let id_raw = unsafe { read_volatile(lapic_id_reg) };
        let local_apic_id = (id_raw >> 24) as u8;

        // Cache I/O APIC version
        let ioapic_version = unsafe { Self::ioapic_read_raw(ioapic_virt_base, IOAPIC_VER) };
        let max_redirection_entry = ((ioapic_version >> 16) & 0xFF) as u8;

        Self {
            lapic_base: lapic_virt_base,
            ioapic_base: ioapic_virt_base,
            local_apic_id,
            ioapic_version,
            max_redirection_entry,
        }
    }

    // ── Local APIC — low‑level register access ──────────────────────

    /// Read a 32‑bit Local APIC register.
    ///
    /// `offset` is a byte offset into the LAPIC MMIO space (e.g.
    /// `ApicOffsets::SPURIOUS_VECTOR`).
    #[inline]
    pub fn lapic_read(&self, offset: u32) -> u32 {
        let addr = (self.lapic_base + offset as u64) as *const u32;
        unsafe { read_volatile(addr) }
    }

    /// Write a 32‑bit value to a Local APIC register.
    #[inline]
    pub fn lapic_write(&self, offset: u32, value: u32) {
        let addr = (self.lapic_base + offset as u64) as *mut u32;
        unsafe { write_volatile(addr, value) }
    }

    // ── Local APIC — control ────────────────────────────────────────

    /// Enable the Local APIC via the spurious‑interrupt vector register.
    ///
    /// Must be called once after construction.  LVTs should be masked
    /// before calling this if the IDT is not yet ready.
    pub fn enable(&self) {
        let spurious = self.lapic_read(ApicOffsets::SPURIOUS_VECTOR);
        self.lapic_write(
            ApicOffsets::SPURIOUS_VECTOR,
            spurious | ApicFlags::SW_ENABLE | 0xFF,
        );
    }

    /// Send End‑Of‑Interrupt.
    #[inline]
    pub fn send_eoi(&self) {
        self.lapic_write(ApicOffsets::EOI, 0);
    }

    /// Mask every Local Vector Table entry (LINT0, LINT1, Error, PMC, Thermal,
    /// Timer).  Use this early in boot before interrupt handlers are installed.
    pub fn mask_all_lvts(&self) {
        let mask: u32 = 1 << 16; // LVT mask bit
        self.lapic_write(ApicOffsets::LVT_LINT0, mask);
        self.lapic_write(ApicOffsets::LVT_LINT1, mask);
        self.lapic_write(ApicOffsets::LVT_ERROR, mask);
        self.lapic_write(ApicOffsets::LVT_PERF_COUNT, mask);
        self.lapic_write(ApicOffsets::LVT_THERMAL, mask);
        self.lapic_write(ApicOffsets::LVT_TIMER, mask);
    }

    /// Configure the LVT timer entry.
    ///
    /// `vector` — interrupt vector number.  
    /// `mode` — one of `ApicFlags::TIMER_ONESHOT` or `ApicFlags::TIMER_PERIODIC`
    ///          (ORed with `ApicFlags::TIMER_MASKED` if desired).  
    /// `initial_count` — value written to the initial‑count register.
    /// `divider` — value written to the divide‑configuration register (0‑7
    ///             where 3 = divide‑by‑16).
    pub fn configure_timer(&self, vector: u32, mode: u32, initial_count: u32, divider: u32) {
        self.lapic_write(ApicOffsets::LVT_TIMER, vector | mode);
        self.lapic_write(ApicOffsets::TMRDIV, divider);
        self.lapic_write(ApicOffsets::TMRINITCNT, initial_count);
    }

    /// Read the timer current‑count register.
    pub fn timer_current_count(&self) -> u32 {
        self.lapic_read(ApicOffsets::TMRCURRCNT)
    }

    /// Return the cached Local APIC ID.
    pub fn local_apic_id(&self) -> u8 {
        self.local_apic_id
    }

    /// Return the Local APIC virtual base address.
    pub fn lapic_base(&self) -> u64 {
        self.lapic_base
    }

    // ── I/O APIC — low‑level register access (private) ──────────────

    /// Raw I/O APIC register read (static helper used during construction).
    unsafe fn ioapic_read_raw(base: u64, reg: u8) -> u32 {
        unsafe {
            let select = base as *mut u32;
            let window = (base + IOAPIC_REG_WINDOW) as *mut u32;
            write_volatile(select, reg as u32);
            read_volatile(window)
        }
    }

    /// Raw I/O APIC register write (static helper used during construction).
    unsafe fn ioapic_write_raw(base: u64, reg: u8, value: u32) {
        unsafe {
            let select = base as *mut u32;
            let window = (base + IOAPIC_REG_WINDOW) as *mut u32;
            write_volatile(select, reg as u32);
            write_volatile(window, value);
        }
    }

    /// Read an I/O APIC register (instance method).
    #[inline]
    fn ioapic_read(&self, reg: u8) -> u32 {
        unsafe { Self::ioapic_read_raw(self.ioapic_base, reg) }
    }

    /// Write an I/O APIC register (instance method).
    #[inline]
    fn ioapic_write(&self, reg: u8, value: u32) {
        unsafe { Self::ioapic_write_raw(self.ioapic_base, reg, value) }
    }

    // ── I/O APIC — public API ───────────────────────────────────────

    /// Read a redirection table entry.
    pub fn read_rte(&self, index: u8) -> IoApicRedirectionEntry {
        IoApicRedirectionEntry {
            lower: self.ioapic_read(IOAPIC_REDTBL_START + index * 2),
            upper: self.ioapic_read(IOAPIC_REDTBL_START + index * 2 + 1),
        }
    }

    /// Write a redirection table entry.
    pub fn write_rte(&self, index: u8, entry: IoApicRedirectionEntry) {
        self.ioapic_write(IOAPIC_REDTBL_START + index * 2, entry.lower);
        self.ioapic_write(IOAPIC_REDTBL_START + index * 2 + 1, entry.upper);
    }

    /// Configure I/O APIC routing for legacy IRQs (keyboard IRQ1 → vector,
    /// mouse IRQ12 → vector).
    ///
    /// This is a convenience method equivalent to the old
    /// `petroleum::configure_io_apic_for_legacy_irqs`.
    pub fn configure_legacy_irqs(&self, keyboard_vector: u8, mouse_vector: u8) {
        let id = self.local_apic_id;

        // Keyboard (IRQ 1)
        let kb_rte =
            IoApicRedirectionEntry::new(keyboard_vector, 0, false, false, false, false, id);
        self.write_rte(1, kb_rte);

        // Mouse (IRQ 12)
        if self.max_redirection_entry >= 12 {
            let mouse_rte =
                IoApicRedirectionEntry::new(mouse_vector, 0, false, false, false, false, id);
            self.write_rte(12, mouse_rte);
        }
    }

    /// Return the cached I/O APIC version register.
    pub fn ioapic_version(&self) -> u32 {
        self.ioapic_version
    }

    /// Return the maximum redirection entry index.
    pub fn max_redirection_entry(&self) -> u8 {
        self.max_redirection_entry
    }

    // ── Legacy PIC ─────────────────────────────────────────────────

    /// Disable the legacy 8259 PIC by remapping its vectors out of the way
    /// and masking all interrupts.  Must be called before enabling the APIC.
    ///
    /// This is a static method — it does not depend on the controller state.
    pub fn disable_legacy_pic() {
        unsafe {
            // ICW1: init
            Port::<u8>::new(PIC_MASTER_COMMAND).write(ICW1_INIT);
            Port::<u8>::new(PIC_SLAVE_COMMAND).write(ICW1_INIT);

            // ICW2: vector offsets
            Port::<u8>::new(PIC_MASTER_DATA).write(0x20); // master: vectors 32‑39
            Port::<u8>::new(PIC_SLAVE_DATA).write(0x28); // slave:  vectors 40‑47

            // ICW3: slave wiring
            Port::<u8>::new(PIC_MASTER_DATA).write(4u8); // slave on IR2
            Port::<u8>::new(PIC_SLAVE_DATA).write(2u8); // identity 2

            // ICW4: 8086 mode
            Port::<u8>::new(PIC_MASTER_DATA).write(ICW4_8086);
            Port::<u8>::new(PIC_SLAVE_DATA).write(ICW4_8086);

            // Mask all interrupts
            Port::<u8>::new(PIC_MASTER_DATA).write(0xFFu8);
            Port::<u8>::new(PIC_SLAVE_DATA).write(0xFFu8);
        }
    }
}

// ── Helper: compute virtual address from physical address ──────────────

/// Convenience helper to turn a physical base address into a higher‑half
/// virtual address using the global `PHYSICAL_MEMORY_OFFSET_BASE`.
///
/// This function is deliberately **not** a method of `ApicController` so that
/// the caller (petroleum / fullerene‑kernel) can compute addresses before
/// constructing the controller.
pub fn phys_to_virt(phys: u64, phys_offset_base: u64) -> u64 {
    phys + phys_offset_base
}
