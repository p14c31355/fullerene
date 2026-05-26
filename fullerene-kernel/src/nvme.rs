//! NVMe (Non-Volatile Memory Express) stub driver.
//!
//! Provides the skeleton for NVMe SSD access via PCI BAR enumeration,
//! admin/completion queue setup, and doorbell register writes.
//! Full implementation requires MSI‑X interrupt routing and PRP/SGL
//! scatter‑gather DMA descriptor construction.

/// Placeholder: initialise NVMe subsystem.
pub fn init() {
    log::info!("NVMe: stub — no NVMe devices enumerated");
}