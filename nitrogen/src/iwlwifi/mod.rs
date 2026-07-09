//! Intel Wireless 7265 (iwlwifi 7000 series) driver.
//!
//! Implements `bonder::NetDevice` with full 802.11 support including
//! firmware loading, TX/RX DMA rings, HCMD interface, scanning, and
//! connection management.
//!
//! ## Module structure
//!
//! - [`regs`] — Register, PCI, and firmware constants
//! - [`types`] — Shared data structures and enums
//! - [`device`] — [`IwlWifiDevice`] struct and core implementation
//! - [`io`] — HCMD, scanning, connection, TX/RX, and trait impls
//! - [`state`] — Global state, incremental init, firmware registry, public API

pub mod regs;
pub mod types;
mod device;
mod io;
mod state;

pub use device::IwlWifiDevice;
pub use io::try_create_iwl;
pub use state::{
    connect_to_ap,
    force_init_failed,
    init_wifi_manager,
    set_wifi_driver_context,
    tick_wifi_device,
    try_init_wifi_device,
    try_init_wifi_device_step,
    wifi_init_completed,
    wifi_state_snapshot,
};
pub use types::WifiManager;
