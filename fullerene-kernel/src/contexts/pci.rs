//! PciContext — PCI device enumeration and lookup.
//!
//! Consolidates:
//! - `nitrogen::pci::PciScanner` usage scattered across multiple modules
//! - ad-hoc `find_device(...)`, `find_hda(...)`, `find_virtio(...)` patterns
//!
//! # Design
//!
//! Instead of scanning PCI buses in each driver's init function, we scan
//! once and store the result.  Drivers query the context:
//!
//! ```rust,ignore
//! let hda = pci.find_device(0x04, 0x03);  // class 4, subclass 3 = HDA
//! let gpu  = pci.find_by_vendor(0x1af4, 0x1050); // VirtIO-GPU
//! ```

use alloc::vec::Vec;
use nitrogen::pci::{PciDevice, PciScanner};
use spin::Mutex;

/// PCI context holding all discovered devices.
pub struct PciContext {
    /// All PCI devices discovered during scan.
    pub devices: Vec<PciDevice>,
}

impl PciContext {
    /// Create a new empty PCI context.
    pub fn new() -> Self {
        Self {
            devices: Vec::new(),
        }
    }

    /// Scan all PCI buses and populate the device list.
    pub fn scan(&mut self) -> Result<(), ()> {
        let mut scanner = PciScanner::new();
        scanner.scan_all_buses()?;
        self.devices.clear();
        self.devices.extend(scanner.get_devices().iter().cloned());
        Ok(())
    }

    /// Populate from an already-scanned `PciScanner`.
    pub fn from_scanner(scanner: &PciScanner) -> Self {
        Self {
            devices: scanner.get_devices().to_vec(),
        }
    }

    /// Return all devices.
    pub fn devices(&self) -> &[PciDevice] {
        &self.devices
    }

    /// Find a device by class code and subclass.
    pub fn find_class(&self, class_code: u8, subclass: u8) -> Option<&PciDevice> {
        self.devices
            .iter()
            .find(|d| d.class_code == class_code && d.subclass == subclass)
    }

    /// Find all devices matching a class code and subclass.
    pub fn find_all_class(&self, class_code: u8, subclass: u8) -> Vec<&PciDevice> {
        self.devices
            .iter()
            .filter(|d| d.class_code == class_code && d.subclass == subclass)
            .collect()
    }

    /// Find a device by vendor ID and device ID.
    pub fn find_by_vendor(&self, vendor_id: u16, device_id: u16) -> Option<&PciDevice> {
        self.devices
            .iter()
            .find(|d| d.vendor_id == vendor_id && d.device_id == device_id)
    }

    /// Find an HDA (High Definition Audio) controller.
    /// Class 0x04 = Multimedia, Subclass 0x03 = HDA.
    pub fn find_hda(&self) -> Option<&PciDevice> {
        self.find_class(0x04, 0x03)
    }

    /// Find a VirtIO-GPU device.
    /// Vendor 0x1AF4 = Red Hat, Device 0x1050 = VirtIO GPU.
    pub fn find_virtio_gpu(&self) -> Option<&PciDevice> {
        self.find_by_vendor(0x1af4, 0x1050)
    }

    /// Find an AHCI SATA controller.
    /// Class 0x01 = Mass Storage, Subclass 0x06 = SATA.
    pub fn find_ahci(&self) -> Option<&PciDevice> {
        self.find_class(0x01, 0x06)
    }

    /// Find an NVMe controller.
    /// Class 0x01 = Mass Storage, Subclass 0x08 = NVMe.
    pub fn find_nvme(&self) -> Option<&PciDevice> {
        self.find_class(0x01, 0x08)
    }

    /// Find a USB xHCI controller.
    /// Class 0x0C = Serial Bus, Subclass 0x03 = USB.
    pub fn find_xhci(&self) -> Option<&PciDevice> {
        self.find_class(0x0c, 0x03)
    }

    /// Len of devices.
    pub fn len(&self) -> usize {
        self.devices.len()
    }

    /// Is the device list empty?
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }
}

/// Global PCI context.
static PCI_CONTEXT: Mutex<Option<PciContext>> = Mutex::new(None);

/// Initialise the global PCI context by scanning buses.
pub fn init_pci_context() -> Result<(), ()> {
    let mut ctx = PciContext::new();
    ctx.scan()?;
    log::info!("PCI: {} devices discovered", ctx.len());
    *PCI_CONTEXT.lock() = Some(ctx);
    Ok(())
}

/// Get a reference to the global PCI context.
pub fn get_pci_context() -> &'static Mutex<Option<PciContext>> {
    &PCI_CONTEXT
}

/// Convenience: execute a closure with a reference to the PCI context.
pub fn with_pci<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&PciContext) -> R,
{
    PCI_CONTEXT.lock().as_ref().map(f)
}