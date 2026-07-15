//! Disk — USB mass-storage block device and StorageManager.
//!
//! [`StorageManager`] owns discovered block-device metadata. Filesystem and
//! mount policy remain in the kernel integration layer.

use alloc::vec::Vec;

// ============================================================================
//  Disk — a discovered USB storage device
// ============================================================================

/// A discovered USB mass-storage disk.
#[derive(Clone)]
pub struct Disk {
    /// Index into the owning host controller's device list.
    pub dev_addr: u8,
    /// Bulk OUT endpoint address.
    pub ep_out: u8,
    /// Bulk OUT endpoint max packet size (bytes).
    pub ep_out_mps: u16,
    /// Bulk IN endpoint address.
    pub ep_in: u8,
    /// Bulk IN endpoint max packet size (bytes).
    pub ep_in_mps: u16,
    /// Block size in bytes (typically 512).
    pub block_size: u32,
    /// Total number of blocks.
    pub total_blocks: u64,
    /// Which controller type ("xHCI" or "EHCI").
    pub ctrl_type: &'static str,
    /// Which controller index (0-based).
    pub ctrl_idx: usize,
}

// ============================================================================
//  StorageManager — owns discovered disks
// ============================================================================

/// Manages the list of discovered USB storage devices.
#[derive(Default)]
pub struct StorageManager {
    disks: Vec<Disk>,
}

impl StorageManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// References to all discovered disks.
    pub fn disks(&self) -> &[Disk] {
        &self.disks
    }

    /// Register a fully enumerated disk, rejecting controller-local duplicates.
    pub fn try_register(&mut self, disk: Disk) -> bool {
        if self.disks.iter().any(|known| {
            known.ctrl_type == disk.ctrl_type
                && known.ctrl_idx == disk.ctrl_idx
                && known.dev_addr == disk.dev_addr
        }) {
            return false;
        }
        self.disks.push(disk);
        true
    }
}
