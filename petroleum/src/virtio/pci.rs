//! Virtio-PCI Capability scanning logic

use crate::hardware::pci::{PciConfigSpace, PciDevice};

// We don't need AltPciDevice here. We can just use the public PciDevice.

#[repr(C, packed)]
pub struct VirtioPciCap {
    pub cap_vndr: u8,
    pub cap_next: u8,
    pub cap_len: u8,
    pub cfg_type: u8,
    pub bar: u8,
    pub padding: [u8; 3],
    pub offset: u32,
    pub length: u32,
}

pub const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
pub const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
pub const VIRTIO_PCI_CAP_ISR_CFG: u8 = 3;
pub const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;

pub fn find_virtio_capability(device: &PciDevice, cfg_type: u8) -> Option<VirtioPciCap> {
    let mut offset = PciConfigSpace::read_config_byte(device.bus, device.device, device.function, 0x34);
    
    while offset != 0 {
        let cap_vndr = PciConfigSpace::read_config_byte(device.bus, device.device, device.function, offset);
        let cap_next = PciConfigSpace::read_config_byte(device.bus, device.device, device.function, offset + 1);
        
        if cap_vndr == 0x09 { // PCI_CAP_ID_VNDR
            let cfg_type_found = PciConfigSpace::read_config_byte(device.bus, device.device, device.function, offset + 3);
            if cfg_type_found == cfg_type {
                let bar = PciConfigSpace::read_config_byte(device.bus, device.device, device.function, offset + 4);
                let cap_offset = PciConfigSpace::read_config_dword(device.bus, device.device, device.function, offset + 8);
                let cap_length = PciConfigSpace::read_config_dword(device.bus, device.device, device.function, offset + 12);
                
                return Some(VirtioPciCap {
                    cap_vndr,
                    cap_next,
                    cap_len: 16,
                    cfg_type: cfg_type_found,
                    bar,
                    padding: [0; 3],
                    offset: cap_offset,
                    length: cap_length,
                });
            }
        }
        offset = cap_next;
    }
    None
}
