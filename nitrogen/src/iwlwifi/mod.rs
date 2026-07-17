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

// Compatibility alias for callers that imported register constants from
// `iwlwifi::regs` before the lifecycle split.
pub use connection_state::{
    connect_to_ap, force_init_failed, init_wifi_manager, set_wifi_driver_context,
    start_scan_if_idle, tick_wifi_device, try_init_wifi_device, try_init_wifi_device_step,
    wifi_init_completed, wifi_state_snapshot,
};
pub use device::IwlWifiDevice;
pub use device::try_create_iwl;
pub use registers as regs;
pub use types::WifiManager;
