//! Bare-metal PCI implementation
use crate::hardware::ports::PortWriter;
use crate::hardware::pci::{PciConfigSpace, PciDevice};

/// PCI configuration space access addresses (x86 I/O ports)
const PCI_CONFIG_ADDR: u16 = 0xCF8;
const PCI_CONFIG_DATA: u16 = 0xCFC;

const PCI_VENDOR_ID_OFFSET: u8 = 0x00;
const PCI_DEVICE_ID_OFFSET: u8 = 0x02;
const PCI_CLASS_CODE_OFFSET: u8 = 0x0B;
const PCI_SUBCLASS_OFFSET: u8 = 0x0A;

/// Helper to build PCI configuration address
pub fn build_pci_config_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x80000000u32
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

/// Read a dword from PCI configuration space
pub fn pci_config_read_dword(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let address = build_pci_config_address(bus, device, function, offset);
    let mut addr_writer = PortWriter::new(PCI_CONFIG_ADDR);
    let mut data_reader = PortWriter::new(PCI_CONFIG_DATA);

    addr_writer.write_safe(address);
    data_reader.read_safe()
}

/// Read a word from PCI configuration space
pub fn pci_config_read_word(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    let dword = pci_config_read_dword(bus, device, function, offset);
    let shift = if offset % 4 < 2 { 0 } else { 16 };
    ((dword >> shift) & 0xFFFF) as u16
}

/// Read a byte from PCI configuration space
pub fn pci_config_read_byte(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    let aligned_register = offset & !0x3; // Align to 32-bit boundary
    let shift = (offset & 0x3) * 8; // 0, 8, 16, or 24
    let dword = pci_config_read_dword(bus, device, function, aligned_register);
    ((dword >> shift) & 0xFF) as u8
}

/// Check if PCI device exists (valid vendor ID)
pub fn pci_device_exists(bus: u8, device: u8, function: u8) -> bool {
    let vendor_id = pci_config_read_word(bus, device, function, PCI_VENDOR_ID_OFFSET);
    vendor_id != 0xFFFF
}

/// Read PCI device information
pub fn read_pci_device_info(
    bus: u8,
    device: u8,
    function: u8,
) -> Option<PciDevice> {
    if !pci_device_exists(bus, device, function) {
        return None;
    }

    let vendor_id = pci_config_read_word(bus, device, function, PCI_VENDOR_ID_OFFSET);
    let device_id = pci_config_read_word(bus, device, function, PCI_DEVICE_ID_OFFSET);
    let class_code = pci_config_read_byte(bus, device, function, PCI_CLASS_CODE_OFFSET);
    let subclass = pci_config_read_byte(bus, device, function, PCI_SUBCLASS_OFFSET);

    let handle = build_pci_config_address(bus, device, function, 0) as usize;

    Some(PciDevice {
        handle,
        vendor_id,
        device_id,
        class_code,
        subclass,
        bus,
        device,
        function,
    })
}

pub fn enumerate_all_pci_devices() -> alloc::vec::Vec<PciDevice> {
    let mut devices = alloc::vec::Vec::new();
    for bus in 0..=255u8 {
        for device in 0..32u8 {
            for function in 0..8u8 {
                if let Some(dev) = read_pci_device_info(bus, device, function) {
                    devices.push(dev);
                }
            }
        }
    }
    devices
}

pub fn enumerate_graphics_devices() -> alloc::vec::Vec<PciDevice> {
    enumerate_all_pci_devices()
        .into_iter()
        .filter(|dev| dev.class_code == 0x03)
        .collect()
}
