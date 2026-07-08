//! Hardware Abstraction Layer
//!
//! This module provides unified hardware abstraction for the Fullerene kernel,
//! including device management, port operations, and hardware interfaces.
//!
//! PCI, port I/O, and interrupt-controller operations now live in the **nitrogen**
//! crate (pure hardware mechanism). This module re-exports them for convenience
//! while keeping the higher-level device-manager policy here.

pub mod device_manager;
pub mod pci_allocator;
