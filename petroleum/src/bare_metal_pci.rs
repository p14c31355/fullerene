use super::*;
use crate::graphics::ports::PortWriter;

/// Macro to reduce repetitive nested loops in PCI enumeration
#[macro_export]
macro_rules! enumerate_pci_devices {
    ($bus:ident, $device:ident, $function:ident, $body:block) => {
        for $bus in 0u8..=255u8 {
            for $device in 0u8..32u8 {
                for $function in 0u8..8u8 {
                    $body
                }
            }
        }
    };
}

/// PCI configuration space access addresses (x86 I/O ports)
const PCI_CONFIG_ADDR: u16 = 0xCF8;
const PCI_CONFIG_DATA: u16 = 0xCFC;

/// PCI configuration space register layout
const PCI_VENDOR_ID_OFFSET: u8 = 0x00;
const PCI_DEVICE_ID_OFFSET: u8 = 0x02;
const PCI_CLASS_CODE_OFFSET: u8 = 0x0B;
const PCI_SUBCLASS_OFFSET: u8 = 0x0A;
const PCI_BAR0_OFFSET: u8 = 0x10;

/// Build PCI configuration address for register access
pub fn build_pci_config_address(bus: u8, device: u8, function: u8, register: u8) -> u32 {
    // PCI config address format: 31=enable, 30-24=reserved, 23-16=bus, 15-11=device, 10-8=function, 7-2=register, 1-0=00
    let addr = (1u32 << 31)
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (register as u32);
    addr & !0x3 // Clear bits 1-0 as they're alignment bits
}

/// Read 32-bit value from PCI configuration space
pub fn pci_config_read_dword(bus: u8, device: u8, function: u8, register: u8) -> u32 {
    let addr = build_pci_config_address(bus, device, function, register);
    let mut addr_writer = PortWriter::new(PCI_CONFIG_ADDR);
    let mut data_reader = PortWriter::new(PCI_CONFIG_DATA);

    addr_writer.write_safe(addr);
    data_reader.read_safe()
}

/// Read 16-bit value with bit alignment to reduce redundant calculations
pub fn pci_config_read_word(bus: u8, device: u8, function: u8, register: u8) -> u16 {
    let aligned_register = register & !3; // Align to 32-bit boundary
    let shift = (register & 0x2) * 8; // 0 or 16
    let dword = pci_config_read_dword(bus, device, function, aligned_register);
    ((dword >> shift) & 0xFFFF) as u16
}

/// Read 8-bit value with bit alignment to reduce redundant calculations
pub fn pci_config_read_byte(bus: u8, device: u8, function: u8, register: u8) -> u8 {
    let aligned_register = register & !0x3; // Align to 32-bit boundary
    let shift = (register & 0x3) * 8; // 0, 8, 16, or 24
    let dword = pci_config_read_dword(bus, device, function, aligned_register);
    ((dword >> shift) & 0xFF) as u8
}

/// Check if PCI device exists (valid vendor ID)
pub fn pci_device_exists(bus: u8, device: u8, function: u8) -> bool {
    let vendor_id = pci_config_read!(bus, device, function, PCI_VENDOR_ID_OFFSET, 16);
    vendor_id != 0xFFFF
}

/// Read PCI device information
pub fn read_pci_device_info(
    bus: u8,
    device: u8,
    function: u8,
) -> Option<crate::graphics_alternatives::PciDevice> {
    if !pci_device_exists(bus, device, function) {
        return None;
    }

    let vendor_id = pci_config_read!(bus, device, function, PCI_VENDOR_ID_OFFSET, 16);
    let device_id = pci_config_read!(bus, device, function, PCI_DEVICE_ID_OFFSET, 16);
    let class_code = pci_config_read!(bus, device, function, PCI_CLASS_CODE_OFFSET, 8);
    let subclass = pci_config_read!(bus, device, function, PCI_SUBCLASS_OFFSET, 8);

    let handle = build_pci_config_address(bus, device, function, 0) as usize;

    Some(crate::graphics_alternatives::PciDevice {
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

/// Read PCI BAR (Base Address Register)
pub fn read_pci_bar(bus: u8, device: u8, function: u8, bar_index: u8) -> u64 {
    // According to the coding rules, reduce code duplication and keep lines of code low
    // This function can potentially support both 32-bit and 64-bit BARs for ANY index
    // not just BAR0. However, for simplicity and to keep the code minimal, we only
    // support 64-bit for BAR0 initially, which is the most common case.
    let offset = PCI_BAR0_OFFSET + (bar_index * 4);
    let bar_low = pci_config_read_dword(bus, device, function, offset);
    let bar_type = bar_low & 0xF;
    let is_64bit = (bar_type & 0x4) != 0;

    // For 64-bit BARs, read the next register for high 32 bits
    let bar_high = if is_64bit && bar_index < 5 {
        // A 64-bit BAR uses two slots, so the last possible start index is 4.
        pci_config_read_dword(bus, device, function, offset + 4)
    } else {
        0
    };

    ((bar_high as u64) << 32) | ((bar_low as u64) & 0xFFFFFFF0)
}

/// Enumerate all PCI devices on all buses
pub fn enumerate_all_pci_devices() -> alloc::vec::Vec<crate::graphics_alternatives::PciDevice> {
    let mut devices = alloc::vec::Vec::new();

    // Scan all possible PCI devices (bus 0-255, device 0-31, function 0-7)
    // In practice, most systems only use bus 0 and maybe a few bridges
    enumerate_pci_devices!(bus, device, function, {
        if let Some(pci_dev) = read_pci_device_info(bus, device, function) {
            if pci_dev.vendor_id != 0xFFFF {
                devices.push(pci_dev);
            }
        }
    });
    // Scan all buses - optimization removed as it may miss devices on secondary buses

    devices
}

/// Find all graphics devices
pub fn enumerate_graphics_devices() -> alloc::vec::Vec<crate::graphics_alternatives::PciDevice> {
    enumerate_all_pci_devices()
        .into_iter()
        .filter(|dev| dev.class_code == 0x03) // Display controller class
        .collect()
}
