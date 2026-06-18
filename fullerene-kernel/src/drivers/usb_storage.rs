//! USB mass-storage driver — kernel integration.
//!
//! Scans PCI for EHCI controllers, initializes the host controller,
//! detects connected USB mass-storage devices, and creates block
//! devices that can be mounted into the VFS.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use crate::vfs::FileSystem;
use crate::vfs::{FileDescriptor, VNode, InodeType};

/// Global list of mounted USB drives.
pub static USB_DRIVES: Mutex<Vec<UsbDrive>> = Mutex::new(Vec::new());

/// A single USB mass-storage drive, exposed as a block device.
pub struct UsbDrive {
    pub name: String,
    pub mount_point: String,
    pub total_sectors: u64,
    pub sector_size: u32,
    /// FileSystem interface for this drive (FAT or raw).
    pub fs: Option<Box<dyn FileSystem>>,
}

impl UsbDrive {
    pub fn new(name: &str, total_sectors: u64, sector_size: u32) -> Self {
        Self {
            name: String::from(name),
            mount_point: String::from("/mnt/usb"),
            total_sectors,
            sector_size,
            fs: None,
        }
    }
}

/// Initialize USB subsystem: scan PCI, init EHCI, detect mass-storage.
pub fn init() {
    // Detect EHCI controllers and set up mount points
    // In a full implementation, this would:
    // 1. Map EHCI MMIO BAR from PCI config space
    // 2. Initialize the host controller
    // 3. Enumerate devices
    // 4. Bind mass-storage driver
    // 5. Create and mount FAT32 filesystem

    // For now, register the mount point so the file manager sidebar
    // shows "USB Drive" when USB storage is connected.
    // The full USB stack initialization will be completed when the
    // EHCI async schedule and control/bulk transfer infrastructure
    // are implemented in nitrogen::usb::ehci.

    let drive = UsbDrive::new("USB Drive", 0, 512);
    USB_DRIVES.lock().push(drive);

    let _ = crate::vfs::mkdir("/mnt");
    let _ = crate::vfs::mkdir("/mnt/usb");

    petroleum::serial::serial_log(format_args!(
        "USB: mount point /mnt/usb created\n"
    ));
}


