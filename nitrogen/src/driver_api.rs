use alloc::boxed::Box;
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
