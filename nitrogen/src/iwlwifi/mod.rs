//! Intel Wireless 7265 (iwlwifi 7000 series) driver.
//!
//! Implements `bonder::NetDevice` with full 802.11 support including
//! firmware loading, TX/RX DMA rings, HCMD interface, scanning, and
//! connection management.
//!
//! ## Module structure
//!
//! - [`registers`] — Register, PCI, and firmware constants
//! - [`types`] — Shared data structures and enums
//! - [`device`] — [`IwlWifiDevice`] struct and core implementation
//! - [`firmware`] — Firmware registry, upload, and alive handling
//! - [`tx`] — Host commands and transmit-ring handling
//! - [`rx`] — Receive-ring and interrupt processing
//! - [`connection_state`] — 802.11 state and high-level public API

mod connection_state;
mod device;
mod firmware;
pub mod registers;
mod rx;
mod tx;
pub mod types;

/// Reinterpret a packed command value as a byte slice.
///
/// # Safety
///
/// `T` must have a stable, padding-free byte layout. The caller must only pass
/// command structures whose representation is explicitly suitable for the
/// firmware protocol.
#[inline(always)]
unsafe fn as_bytes<T: Sized>(value: &T) -> &[u8] {
    // SAFETY: enforced by this function's contract.
    unsafe {
        core::slice::from_raw_parts(value as *const T as *const u8, core::mem::size_of::<T>())
    }
}

// Compatibility alias for callers that imported register constants from
// `iwlwifi::regs` before the lifecycle split.
pub use connection_state::{
    connect_to_ap, consume_wifi_completion_queue, consume_wifi_completion_queue_until,
    force_init_failed, init_wifi_manager, process_wifi_submission_queue,
    process_wifi_submission_queue_until, retry_wifi_initialization, set_wifi_driver_context,
    start_scan_if_idle, tick_wifi_device, try_init_wifi_device, try_init_wifi_device_step,
    wifi_init_completed, wifi_state_snapshot,
};
pub use device::IwlWifiDevice;
pub use device::try_create_iwl;
pub use registers as regs;
pub use types::WifiManager;
