//! I/O APIC convenience — policy-level init (kernel abstraction layer)
//!
//! These functions configure interrupt routing and are NOT part of the
//! pure hardware mechanism layer (`nitrogen`). They live here as a
//! convenience for kernel consumers and delegate to `nitrogen::ioapic`.
//!
//! NOTE: Eventually these should move to `fullerene-kernel` or the caller.

use nitrogen::ioapic::{IoApic, IoApicRedirectionEntry};

/// Configure I/O APIC for legacy IRQs
pub fn configure_io_apic_for_legacy_irqs(io_apic: &mut IoApic, local_apic_id: u8) {
    // Configure keyboard (IRQ 1) -> vector 33
    let keyboard_rte =
        IoApicRedirectionEntry::new(33, 0, false, false, false, false, local_apic_id);
    io_apic.write_rte(1, keyboard_rte);

    // Configure mouse (IRQ 12) -> vector 44
    let mouse_rte = IoApicRedirectionEntry::new(44, 0, false, false, false, false, local_apic_id);
    io_apic.write_rte(12, mouse_rte);
}

/// Get local APIC ID from the LAPIC
pub unsafe fn get_local_apic_id(lapic_base: u64) -> u8 {
    unsafe { nitrogen::ioapic::get_local_apic_id(lapic_base) }
}

/// Initialize I/O APIC for legacy interrupts
pub fn init_io_apic(lapic_base: u64, io_apic_base: u64) {
    let local_apic_id = unsafe { get_local_apic_id(lapic_base) };
    let mut io_apic = IoApic::new(io_apic_base);
    configure_io_apic_for_legacy_irqs(&mut io_apic, local_apic_id);
}