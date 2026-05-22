//! Virtio module for Nitrogen — pure hardware mechanism.
//!
//! Sub-modules:
//! - `cap` : VirtIO PCI capability scanning
//! - `gpu` : VirtIO-GPU driver (caller provides physical memory)

pub mod cap;
pub mod gpu;
