//! Virtio - re-exported from nitrogen.
//!
//! NOTE: `pci` is an alias for `cap` for backward compatibility with
//! existing kernel code that references `petroleum::virtio::pci`.


/// Compatibility alias – `pci` now refers to `cap` (VirtIO PCI capability scanning).
pub mod pci {
    pub use nitrogen::virtio::cap::*;
}
