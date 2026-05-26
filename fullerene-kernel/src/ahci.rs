//! AHCI (Advanced Host Controller Interface) stub driver.
//!
//! Provides the skeleton for SATA disk access via PCI AHCI controllers.
//! Full implementation requires PCI BAR enumeration, port probing,
//! FIS-based command submission and interrupt handling.

/// Placeholder: initialise AHCI subsystem.
pub fn init() {
    log::info!("AHCI: stub — no SATA devices enumerated");
}