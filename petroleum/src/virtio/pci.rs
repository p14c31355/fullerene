//! Virtio-PCI Capability scanning logic

use alloc::vec::Vec;
use crate::hardware::pci::{PciConfigSpace, PciDevice};

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
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
pub const VIRTIO_PCI_CAP_PCI_CFG: u8 = 5; // VirtIO 1.0+ spec defines this as Type 5, not 6. Wait, check spec again. Actually, it's type 5. Let me check. Ah, it is 5. Wait, the spec says PCI_CFG is type 5. I will add type 5.

pub fn find_virtio_capability(device: &PciDevice, cfg_type: u8) -> Option<VirtioPciCap> {
    get_virtio_caps(device).into_iter().find(|cap| cap.cfg_type == cfg_type)
}

pub fn get_virtio_caps(device: &PciDevice) -> Vec<VirtioPciCap> {
    let mut caps = Vec::new();
    let mut offset =
        PciConfigSpace::read_config_byte(device.bus, device.device, device.function, 0x34);
    
    while offset != 0 && offset != 0xFF {
        let cap_vndr =
            PciConfigSpace::read_config_byte(device.bus, device.device, device.function, offset);
        let cap_next =
            PciConfigSpace::read_config_byte(device.bus, device.device, device.function, offset + 1);

        if cap_vndr == 0x09 {
            // Vendor-Specific
            let cfg_type = PciConfigSpace::read_config_byte(device.bus, device.device, device.function, offset + 3);
            let bar = PciConfigSpace::read_config_byte(device.bus, device.device, device.function, offset + 4);
            let cap_offset = PciConfigSpace::read_config_dword(device.bus, device.device, device.function, offset + 8);
            let cap_length = PciConfigSpace::read_config_dword(device.bus, device.device, device.function, offset + 12);
            
            caps.push(VirtioPciCap {
                cap_vndr,
                cap_next,
                cap_len: 16,
                cfg_type,
                bar,
                padding: [0; 3],
                offset: cap_offset,
                length: cap_length,
            });
        }
        offset = cap_next;
    }
    caps
}

pub fn dump_capabilities(device: &PciDevice) {
    let mut offset =
        PciConfigSpace::read_config_byte(device.bus, device.device, device.function, 0x34);
    
    crate::serial::_print(format_args!("[PCI] Dumping capabilities starting at {:#x}\n", offset));
    
    while offset != 0 && offset != 0xFF {
        let cap_id = PciConfigSpace::read_config_byte(device.bus, device.device, device.function, offset);
        let cap_next = PciConfigSpace::read_config_byte(device.bus, device.device, device.function, offset + 1);
        
        crate::serial::_print(format_args!("[PCI] Cap ID: {:#x}, Next: {:#x}, Offset: {:#x}\n", cap_id, cap_next, offset));
        
        if cap_id == 0x09 {
            // Vendor-Specific
            let cfg_type = PciConfigSpace::read_config_byte(device.bus, device.device, device.function, offset + 3);
            let bar = PciConfigSpace::read_config_byte(device.bus, device.device, device.function, offset + 4);
            let cap_offset = PciConfigSpace::read_config_dword(device.bus, device.device, device.function, offset + 8);
            let cap_len = PciConfigSpace::read_config_dword(device.bus, device.device, device.function, offset + 12);
            
            crate::serial::_print(format_args!(
                "  -> VirtIO VNDR: type={}, bar={}, offset={:#x}, len={:#x}\n",
                cfg_type, bar, cap_offset, cap_len
            ));
        }
        
        offset = cap_next;
    }
}
