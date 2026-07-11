//! Disk — USB mass-storage block device and StorageManager.
//!
//! [`StorageManager`] owns all discovered disks and handles mounting
//! to the kernel's VFS.  Each [`Disk`] wraps a FAT filesystem mounted
//! at a `/mnt/usb-N` path.

use alloc::string::String;
use alloc::vec::Vec;

// ============================================================================
//  Disk — a mounted USB storage device
// ============================================================================

/// A mounted USB mass-storage disk.
#[derive(Clone)]
pub struct Disk {
    /// Human-readable name (e.g. "USB Drive 1").
    pub name: String,
    /// VFS mount point (e.g. "/mnt/usb-1").
    pub mount_point: String,
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
//  StorageManager — owns all disks, handles auto-mount
// ============================================================================

/// Manages the list of mounted USB storage devices.
pub struct StorageManager {
    disks: Vec<Disk>,
}

impl StorageManager {
    pub fn new() -> Self {
        Self { disks: Vec::new() }
    }

    /// References to all mounted disks.
    pub fn disks(&self) -> &[Disk] {
        &self.disks
    }

    /// Try to mount a mass-storage device using the given endpoint info.
    ///
    /// The actual BPB parsing and VFS mounting is delegated to the
    /// platform callback via [`platform_mount_fat`].
    pub fn try_mount(
        &mut self,
        ctrl_type: &'static str,
        dev_addr: u8,
        ep_out: u8,
        ep_in: u8,
        ctrl_idx: usize,
    ) -> bool {
        self.try_mount_with_mps(ctrl_type, dev_addr, ep_out, 512, ep_in, 512, ctrl_idx)
    }

    /// Try to mount a mass-storage device with the given endpoint info
    /// and per-endpoint max packet sizes.  `ep_out_mps` and `ep_in_mps`
    /// are the device-reported wMaxPacketSize for each bulk endpoint.
    pub fn try_mount_with_mps(
        &mut self,
        ctrl_type: &'static str,
        dev_addr: u8,
        ep_out: u8,
        ep_out_mps: u16,
        ep_in: u8,
        ep_in_mps: u16,
        ctrl_idx: usize,
    ) -> bool {
        let disk_num = self.disks.len() + 1;
        let name = alloc::format!("USB Drive {}", disk_num);
        let mount_point = alloc::format!("/mnt/usb-{}", disk_num);

        let disk = Disk {
            name,
            mount_point,
            dev_addr,
            ep_out,
            ep_out_mps,
            ep_in,
            ep_in_mps,
            block_size: 512,
            total_blocks: 0,
            ctrl_type,
            ctrl_idx,
        };

        if self.disks.iter().any(|known| {
            known.ctrl_type == ctrl_type && known.ctrl_idx == ctrl_idx && known.dev_addr == dev_addr
        }) {
            return false;
        }
        self.disks.push(disk);
        true
    }
}
