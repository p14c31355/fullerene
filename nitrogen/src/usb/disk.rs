//! Disk — USB mass-storage block device and StorageManager.
//!
//! [`StorageManager`] owns all discovered disks and handles mounting
//! to the kernel's VFS.  Each [`Disk`] wraps a FAT filesystem mounted
//! at a `/mnt/usb-N` path.

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

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
    /// Bulk IN endpoint address.
    pub ep_in: u8,
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
        let disk_num = self.disks.len() + 1;
        let name = alloc::format!("USB Drive {}", disk_num);
        let mount_point = alloc::format!("/mnt/usb-{}", disk_num);

        let disk = Disk {
            name,
            mount_point,
            dev_addr,
            ep_out,
            ep_in,
            block_size: 512,
            total_blocks: 0,
            ctrl_type,
            ctrl_idx,
        };

        let ok = platform_mount_fat(&disk);
        if ok {
            self.disks.push(disk);
        }
        ok
    }
}

// ── Platform-specific FAT mounting ─────────────────────────

/// Thin bridge: the kernel crate provides the actual
/// [`crate::drivers::fat::FatFileSystem`] implementation.
///
/// This function is called by [`StorageManager::try_mount`] and is
/// expected to be defined by the kernel crate via a `#[no_mangle]`
/// override or by calling [`set_mount_fn`] during early init.
/// Platform callback: the kernel crate registers this to mount a
/// FAT filesystem from the given disk's parameters.  The callback
/// is responsible for all VFS interactions.
static MOUNT_FN: spin::Mutex<Option<fn(&Disk) -> bool>> =
    spin::Mutex::new(None);

/// Register the platform's FAT-mount callback.
pub fn set_mount_fn(f: fn(&Disk) -> bool) {
    *MOUNT_FN.lock() = Some(f);
}

fn platform_mount_fat(disk: &Disk) -> bool {
    let f = MOUNT_FN.lock();
    match f.as_ref() {
        Some(f) => f(disk),
        None => {
            log::warn!("USB: no mount callback registered; disk not mounted");
            false
        }
    }
}
