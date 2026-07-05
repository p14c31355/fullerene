//! Minimal legacy PCI configuration access used by VGA detection.

use nitrogen::port::PortWriter;

const PCI_CONFIG_ADDR: u16 = 0xCF8;
const PCI_CONFIG_DATA: u16 = 0xCFC;

fn config_address(bus: u8, device: u8, function: u8, register: u8) -> u32 {
    (1 << 31)
        | (u32::from(bus) << 16)
        | (u32::from(device) << 11)
        | (u32::from(function) << 8)
        | u32::from(register & !3)
}

pub fn pci_config_read_dword(bus: u8, device: u8, function: u8, register: u8) -> u32 {
    let mut address = PortWriter::new(PCI_CONFIG_ADDR);
    let mut data = PortWriter::new(PCI_CONFIG_DATA);
    address.write_safe(config_address(bus, device, function, register));
    data.read_safe()
}

pub fn pci_config_read_word(bus: u8, device: u8, function: u8, register: u8) -> u16 {
    let value = pci_config_read_dword(bus, device, function, register);
    (value >> (u32::from(register & 2) * 8)) as u16
}
