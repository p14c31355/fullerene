//! PciContext — replaces ad-hoc PciScanner usage.
use alloc::vec::Vec;
use nitrogen::pci::{PciDevice, PciScanner};
use spin::Mutex;

pub struct PciContext {
    pub devices: Vec<PciDevice>,
}

impl PciContext {
    pub fn new() -> Self {
        Self {
            devices: Vec::new(),
        }
    }
    pub fn scan(&mut self) -> Result<(), ()> {
        let mut s = PciScanner::new();
        s.scan_all_buses()?;
        self.devices = s.get_devices().to_vec();
        Ok(())
    }
    pub fn devices(&self) -> &[PciDevice] {
        &self.devices
    }
    pub fn find_class(&self, class: u8, sub: u8) -> Option<&PciDevice> {
        self.devices
            .iter()
            .find(|d| d.class_code == class && d.subclass == sub)
    }
    pub fn find_by_vendor(&self, vid: u16, did: u16) -> Option<&PciDevice> {
        self.devices
            .iter()
            .find(|d| d.vendor_id == vid && d.device_id == did)
    }
    pub fn find_hda(&self) -> Option<&PciDevice> {
        self.find_class(0x04, 0x03)
    }
    pub fn find_virtio_gpu(&self) -> Option<&PciDevice> {
        self.find_by_vendor(0x1af4, 0x1050)
    }
    pub fn find_ahci(&self) -> Option<&PciDevice> {
        self.find_class(0x01, 0x06)
    }
    pub fn find_nvme(&self) -> Option<&PciDevice> {
        self.find_class(0x01, 0x08)
    }
    pub fn find_xhci(&self) -> Option<&PciDevice> {
        self.find_class(0x0c, 0x03)
    }
}

static PCI_CTX: Mutex<Option<PciContext>> = Mutex::new(None);
pub fn init_pci() {
    let mut c = PciContext::new();
    let _ = c.scan();
    *PCI_CTX.lock() = Some(c);
}
pub fn get_pci() -> &'static Mutex<Option<PciContext>> {
    &PCI_CTX
}
pub fn with_pci<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&PciContext) -> R,
{
    PCI_CTX.lock().as_ref().map(f)
}
