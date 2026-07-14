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

impl DriverBox {
    /// Finalise initialisation after probe — equivalent to calling
    /// `init()` on the inner driver.
    ///
    /// This is the **attach** step in the probe → priority → attach →
    /// driver-manager pipeline.
    pub fn attach(&mut self) -> Result<(), &'static str> {
        match self {
            DriverBox::Storage(d) => d.init(),
            DriverBox::Network(d) => d.init(),
            DriverBox::Display(d) => d.init(),
            DriverBox::Audio(d) => d.init(),
            DriverBox::UsbHost(d) => d.init(),
            DriverBox::None => Ok(()),
        }
    }
}

/// Entry point every PCI-subclass driver plugin exports.
pub type PluginEntry = fn(device: &PciDevice) -> DriverBox;

/// ── DriverDescriptor — PCI identity for matching ───────────────

/// Describes the hardware a driver can handle.
///
/// Each driver publishes one of these; the registry uses it to match
/// against discovered PCI devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverDescriptor {
    /// PCI vendor ID (0xFFFF = wildcard).
    pub vendor: u16,
    /// PCI device ID (0xFFFF = wildcard; ignored when vendor is wildcard).
    pub device: u16,
    /// PCI class code (0xFF = wildcard).
    pub class: u8,
    /// PCI subclass (only meaningful when class is not wildcard).
    pub subclass: u8,
}

impl DriverDescriptor {
    /// A wildcard descriptor that matches any device.
    pub const fn wildcard() -> Self {
        Self {
            vendor: 0xFFFF,
            device: 0xFFFF,
            class: 0xFF,
            subclass: 0xFF,
        }
    }

    /// Build from vendor/device pair.
    pub const fn from_vid_did(vendor: u16, device: u16) -> Self {
        Self {
            vendor,
            device,
            class: 0xFF,
            subclass: 0xFF,
        }
    }

    /// Build from class/subclass pair.
    pub const fn from_class(class: u8, subclass: u8) -> Self {
        Self {
            vendor: 0xFFFF,
            device: 0xFFFF,
            class,
            subclass,
        }
    }

    /// Returns `true` if this descriptor matches the given PCI device.
    pub fn matches(&self, device: &PciDevice) -> bool {
        // Exact vendor/device match
        if self.vendor != 0xFFFF
            && self.device != 0xFFFF
            && self.vendor == device.vendor_id
            && self.device == device.device_id
        {
            return true;
        }
        // Class/subclass match
        if self.class != 0xFF
            && self.class == device.class_code
            && self.subclass == device.subclass
        {
            return true;
        }
        // Wildcard fallback (both vendor and class are wildcard)
        self.vendor == 0xFFFF && self.device == 0xFFFF && self.class == 0xFF
    }
}

/// ── Driver lifecycle trait ─────────────────────────────────────
///
/// A `Driver` knows its PCI identity via [`descriptor`](Self::descriptor)
/// and can produce a type-erased driver instance when probed against a
/// real device.
///
/// The kernel never knows concrete driver names — it only holds
/// `Box<dyn Driver>` entries in the [`DriverRegistry`].  Adding a new
/// driver means implementing this trait and calling `registry.register()`;
/// no kernel source file needs to change.
pub trait Driver: Send {
    /// Return the hardware descriptor used for PCI matching.
    ///
    /// The default implementation builds a descriptor from [`pci_id`](Self::pci_id)
    /// and [`pci_class`](Self::pci_class) for backward compatibility.
    fn descriptor(&self) -> DriverDescriptor {
        let (vid, did) = self.pci_id();
        let class = self.pci_class();
        match class {
            Some((c, s)) => DriverDescriptor {
                vendor: vid,
                device: did,
                class: c,
                subclass: s,
            },
            None => DriverDescriptor {
                vendor: vid,
                device: did,
                class: 0xFF,
                subclass: 0xFF,
            },
        }
    }

    /// PCI vendor/device pair this driver handles.
    ///
    /// Return `(0xFFFF, 0xFFFF)` for a fallback driver that matches any
    /// device (the registry tries fallback drivers last).
    ///
    /// **Note**: new drivers should override [`descriptor`](Self::descriptor)
    /// instead.  This method is kept for backward compatibility.
    fn pci_id(&self) -> (u16, u16) {
        (0xFFFF, 0xFFFF)
    }

    /// PCI class/subclass this driver handles.
    ///
    /// Override this for generic drivers that match by device class
    /// (e.g. AHCI = class 0x01/subclass 0x06) instead of vendor/device.
    /// Return `None` to skip class‑based matching (default).
    ///
    /// **Note**: new drivers should override [`descriptor`](Self::descriptor)
    /// instead.  This method is kept for backward compatibility.
    fn pci_class(&self) -> Option<(u8, u8)> {
        None
    }

    /// Probing priority — higher values are tried first when multiple
    /// drivers match the same device.
    ///
    /// Override this to influence driver selection order (e.g. a more
    /// specific driver returns a higher priority than a generic one).
    fn priority(&self) -> i32 {
        0
    }

    /// Probe a PCI device and return a type-erased driver instance.
    ///
    /// The default calls [`attach`](Self::attach) if the probe succeeds,
    /// but drivers may override to return a pre-configured instance.
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
    /// Strategy: collect all matching drivers, sort by [`priority`](Driver::priority)
    /// (highest first), and call [`probe`](Driver::probe) on each until one
    /// returns a non-`None` instance.
    pub fn match_device(
        &self,
        ctx: &dyn DriverContext,
        device: &PciDevice,
    ) -> DriverBox {
        // Collect matching candidates with their priority.
        let mut candidates: Vec<(usize, i32)> = Vec::new();
        for (i, (_name, driver)) in self.drivers.iter().enumerate() {
            if driver.descriptor().matches(device) {
                candidates.push((i, driver.priority()));
            }
        }

        // Sort by priority descending.
        candidates.sort_by(|a, b| b.1.cmp(&a.1));

        // Try each candidate's probe in priority order.
        for (idx, _prio) in &candidates {
            let (_name, driver) = &self.drivers[*idx];
            let result = driver.probe(ctx, device);
            if !matches!(result, DriverBox::None) {
                return result;
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
