//! USB mass-storage driver — kernel integration.
//!
//! Scans PCI for EHCI controllers, detects connected USB mass-storage
//! devices, creates block devices, mounts FAT/exFAT filesystems,
//! and registers them for the file manager sidebar.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use crate::drivers::fat::{BlockDevice, FatFileSystem};

/// Global list of mounted USB drives.
pub static USB_DRIVES: Mutex<Vec<UsbDrive>> = Mutex::new(Vec::new());

/// A mounted USB drive visible in the file manager sidebar.
pub struct UsbDrive {
    pub name: String,
    pub mount_point: String,
    pub total_sectors: u64,
    pub sector_size: u32,
}

impl UsbDrive {
    pub fn new(name: &str, mount_point: &str) -> Self {
        Self {
            name: String::from(name),
            mount_point: String::from(mount_point),
            total_sectors: 0,
            sector_size: 512,
        }
    }
}

/// Initialize USB subsystem: detect mass-storage, mount filesystems.
pub fn init() {
    // ── Real USB detection (deferred until EHCI schedule is implemented) ──
    //
    // The full flow will be:
    //
    // 1. PCI scan for EHCI controllers (class=0x0C, subclass=0x03, prog_if=0x20)
    // 2. Map MMIO BAR, initialize EHCI, start async schedule
    // 3. Poll root hub ports for connection
    // 4. Enumerate device → get device descriptor
    // 5. If mass-storage class (0x08), configure bulk endpoints
    // 6. Create UsbMassStorage → UsbBlockDevice → FatFileSystem
    // 7. vfs::mount("/mnt/usb", fat_fs)
    // 8. Register UsbDrive in USB_DRIVES
    //
    // Steps 2-6 require the EHCI async schedule for control/bulk transfers,
    // which is not yet implemented in nitrogen::usb::ehci.
    //
    // Until then, NO fake mount point is created, so the file manager sidebar
    // will not show a misleading "USB Drive" item.

    // Ensure /mnt exists (for future mounting)
    let _ = crate::vfs::mkdir("/mnt");

    petroleum::serial::serial_log(format_args!(
        "USB: subsystem ready (EHCI async schedule pending)\n"
    ));
}

/// Mount a USB block device at the given mount point.
///
/// Called when a real USB mass-storage device is detected.
/// Detects FAT32 vs exFAT from the boot sector and mounts accordingly.
pub fn mount_usb(
    name: &str,
    mount_point: &str,
    device: Box<dyn BlockDevice>,
) -> Result<(), &'static str> {
    // Create and mount FAT/exFAT filesystem
    let fat_fs = FatFileSystem::new(device)?;

    let _ = crate::vfs::mkdir(mount_point);
    // Mount into VFS via VfsContext
    crate::contexts::vfs::with_vfs(|vfs_ctx| {
        vfs_ctx.mount(mount_point, Box::new(fat_fs))
    }).ok_or("vfs not init")??;

    // Register for file manager sidebar
    let drive = UsbDrive::new(name, mount_point);
    USB_DRIVES.lock().push(drive);

    petroleum::serial::serial_log(format_args!(
        "USB: mounted '{}' at {}\n", name, mount_point
    ));
    Ok(())
}
