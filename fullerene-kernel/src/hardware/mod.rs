//! Hardware Abstraction Layer
//!
//! This module provides unified hardware abstraction for the Fullerene kernel,
//! including device management, port operations, and hardware interfaces.
//!
//! PCI and port I/O operations are now consolidated in the petroleum crate.

pub mod device_manager;

// Re-export commonly used types
pub use device_manager::{DeviceInfo, DeviceManager, init_device_manager, register_device};
pub use petroleum::HardwarePorts;
pub use petroleum::hardware::VgaRegisterWriter;
pub use petroleum::hardware::{PciConfigSpace, PciDevice, PciScanner};
