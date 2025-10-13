//! Hardware Abstraction Layer
//!
//! This module provides unified hardware abstraction for the Fullerene kernel,
//! including device management, port operations, and hardware interfaces.

pub mod device_manager;
pub mod pci;
pub mod ports;

// Re-export commonly used types
pub use device_manager::{DeviceManager, DeviceInfo, init_device_manager, register_device, register_vga_device};
pub use pci::{PciDevice, PciConfigSpace};
pub use ports::{HardwarePorts, VgaRegisterWriter, convenience};
