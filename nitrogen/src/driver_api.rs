use alloc::boxed::Box;
use alloc::vec::Vec;
use crate::DriverContext;
use crate::pci::PciDevice;

/// Block-device interface for mass-storage controllers
/// (NVMe, AHCI, SATA, IDE, SD/MMC, USB mass storage, etc.).
pub trait StorageDriver: Send {
    fn init(&mut self) -> Result<(), &'static str>;
    fn read_blocks(&self, lba: u64, count: usize, buf: &mut [u8]) -> Result<(), &'static str>;
    fn write_blocks(&self, lba: u64, count: usize, buf: &[u8]) -> Result<(), &'static str>;
    fn block_size(&self) -> u32;
    fn total_blocks(&self) -> u64;
}

/// Network interface controller (Ethernet, Wi-Fi, etc.).
pub trait NetworkDriver: Send {
    fn init(&mut self) -> Result<(), &'static str>;
    fn send(&self, buf: &[u8]) -> Result<(), &'static str>;
    fn receive(&self, buf: &mut [u8]) -> Result<usize, &'static str>;
    fn mac_address(&self) -> [u8; 6];
}

/// Display / GPU controller (VGA-compatible, VirtIO-GPU, etc.).
pub trait DisplayDriver: Send {
    fn init(&mut self) -> Result<(), &'static str>;
    fn framebuffer(&self) -> &[u8];
    fn resolution(&self) -> (usize, usize);
    fn stride(&self) -> usize;
    fn flush(&self);
}

/// Audio controller (HDA, AC97, USB audio, etc.).
pub trait AudioDriver: Send {
    fn init(&mut self) -> Result<(), &'static str>;
    fn play(&self, buf: &[u8]) -> Result<(), &'static str>;
}

/// USB host controller (EHCI, XHCI, OHCI, UHCI).
pub trait UsbHostDriver: Send {
    fn init(&mut self) -> Result<(), &'static str>;
    fn poll(&self);
}

/// Type-erased return from a plugin entry point.
///
/// The kernel matches the returned variant against the PCI device's
/// (class, subclass) to dispatch the correct driver API.
pub enum DriverBox {
    Storage(Box<dyn StorageDriver>),
    Network(Box<dyn NetworkDriver>),
    Display(Box<dyn DisplayDriver>),
    Audio(Box<dyn AudioDriver>),
    UsbHost(Box<dyn UsbHostDriver>),
    None,
}

/// Entry point every PCI-subclass driver plugin exports.
pub type PluginEntry = fn(device: &PciDevice) -> DriverBox;

/// ── Driver lifecycle trait ─────────────────────────────────────
///
/// A `Driver` knows its PCI vendor/device identity and can produce
/// a type-erased driver instance when probed against a real device.
///
/// The kernel never knows concrete driver names — it only holds
/// `Box<dyn Driver>` entries in the [`DriverRegistry`].  Adding a new
/// driver means implementing this trait and calling `registry.register()`;
/// no kernel source file needs to change.
pub trait Driver: Send {
    /// PCI vendor/device pair this driver handles.
    ///
    /// Return `(0xFFFF, 0xFFFF)` for a fallback driver that matches any
    /// device (the registry tries fallback drivers last).
    fn pci_id(&self) -> (u16, u16) {
        (0xFFFF, 0xFFFF)
    }

    /// PCI class/subclass this driver handles.
    ///
    /// Override this for generic drivers that match by device class
    /// (e.g. AHCI = class 0x01/subclass 0x06) instead of vendor/device.
    /// Return `None` to skip class‑based matching (default).
    fn pci_class(&self) -> Option<(u8, u8)> {
        None
    }

    /// Probe a PCI device and return a type-erased driver instance.
    fn probe(&self, ctx: &dyn DriverContext, device: &PciDevice) -> DriverBox;
}

/// ── Driver registry ────────────────────────────────────────────
///
/// A collection of [`Driver`] instances keyed by PCI identification.
/// The kernel populates this at boot, then queries it during PCI
/// enumeration via [`match_device`](Self::match_device).
#[derive(Default)]
pub struct DriverRegistry {
    drivers: Vec<(&'static str, Box<dyn Driver>)>,
}

impl DriverRegistry {
    pub fn new() -> Self {
        Self {
            drivers: Vec::new(),
        }
    }

    /// Register a driver under a human‑readable name.
    pub fn register(&mut self, name: &'static str, driver: Box<dyn Driver>) {
        self.drivers.push((name, driver));
    }

    /// Match a PCI device against all registered drivers.
    ///
    /// Three-pass strategy:
    /// 1. Exact vendor/device match
    /// 2. Class/subclass match (e.g. AHCI = 0x01/0x06)
    /// 3. Fallback drivers (`0xFFFF, 0xFFFF`)
    pub fn match_device(
        &self,
        ctx: &dyn DriverContext,
        device: &PciDevice,
    ) -> DriverBox {
        for (_name, driver) in &self.drivers {
            let (vid, did) = driver.pci_id();
            if vid == device.vendor_id && did == device.device_id {
                let result = driver.probe(ctx, device);
                if !matches!(result, DriverBox::None) {
                    return result;
                }
            }
        }
        // Second pass: class/subclass match (e.g. AHCI = 0x01/0x06).
        for (_name, driver) in &self.drivers {
            if let Some((class, subclass)) = driver.pci_class() {
                if class == device.class_code && subclass == device.subclass {
                    let result = driver.probe(ctx, device);
                    if !matches!(result, DriverBox::None) {
                        return result;
                    }
                }
            }
        }
        // Third pass: fallback drivers (0xFFFF, 0xFFFF) that do NOT override pci_class.
        // Drivers that override pci_class() are class-based matchers, not fallback.
        for (_name, driver) in &self.drivers {
            let (vid, did) = driver.pci_id();
            if vid == 0xFFFF && did == 0xFFFF && driver.pci_class().is_none() {
                let result = driver.probe(ctx, device);
                if !matches!(result, DriverBox::None) {
                    return result;
                }
            }
        }
        DriverBox::None
    }

    /// Iterate over registered driver names.
    pub fn iter(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.drivers.iter().map(|(name, _)| *name)
    }

    pub fn len(&self) -> usize {
        self.drivers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.drivers.is_empty()
    }
}
