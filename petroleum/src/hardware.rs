//! Hardware Abstraction Layer
//!
//! This module consolidates hardware-specific functionality from across
//! subcrates to reduce code duplication and enable reuse.
//!
//! It includes:
//! - PCI configuration space access
//! - Generic port I/O operations
//! - Hardware device management

pub mod pci;
pub mod ports;

/// Re-export commonly used hardware types
pub use pci::{PciConfigSpace, PciDevice, PciScanner};
pub use ports::{VgaRegisterWriter};
pub use crate::graphics::HardwarePorts;
