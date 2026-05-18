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

/// Extended capability structure for PCI Configuration Access (Type 5)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct VirtioPciCfgCap {
    pub cap_vndr: u8,
    pub cap_next: u8,
    pub cap_len: u8,
    pub cfg_type: u8,      // Must be 5
    pub bar: u8,
    pub padding: [u8; 3],
    pub offset: u32,
    pub length: u32,
    /// PCI CFG specific field: data register (at offset 0x14)
    pub pci_cfg_data: [u8; 4],
}

pub const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
pub const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
pub const VIRTIO_PCI_CAP_ISR_CFG: u8 = 3;
pub const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;
pub const VIRTIO_PCI_CAP_PCI_CFG: u8 = 5; // PCI Configuration Access Capability (Type 5)

/// Find a Virtio capability by type, returning the old-style cap (without PCI CFG fields)
pub fn find_virtio_capability(device: &PciDevice, cfg_type: u8) -> Option<VirtioPciCap> {
    get_virtio_caps(device).into_iter().find(|cap| cap.cfg_type == cfg_type).map(|cap| VirtioPciCap {
        cap_vndr: cap.cap_vndr,
        cap_next: cap.cap_next,
        cap_len: 16, // Original length without PCI CFG fields
        cfg_type: cap.cfg_type,
        bar: cap.bar,
        padding: [0; 3],
        offset: cap.offset,
        length: cap.length,
    })
}

/// Get all Virtio capabilities with full PCI CFG support
pub fn get_virtio_caps(device: &PciDevice) -> Vec<VirtioPciCfgCap> {
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
            let pci_cfg_data = PciConfigSpace::read_config_dword(device.bus, device.device, device.function, offset + 20).to_le_bytes();
            
            caps.push(VirtioPciCfgCap {
                cap_vndr,
                cap_next,
                cap_len: 20,
                cfg_type,
                bar,
                padding: [0; 3],
                offset: cap_offset,
                length: cap_length,
                pci_cfg_data,
            });
        }
        offset = cap_next;
    }
    caps
}

/// Read a register from the device via PCI Configuration Access Capability (Type 5)
/// 
/// This is used when direct BAR mapping is not working (e.g., in QEMU with OVMF).
/// 
/// The `width` parameter specifies the data width: 1 (byte), 2 (word), or 4 (dword).
pub fn read_virtio_reg_via_pci_cfg(
    device: &PciDevice,
    bar: u8,
    offset: u32,
    width: u32,   // 1, 2, or 4 bytes
) -> Option<u32> {
    let caps = get_virtio_caps(device);
    let cap = caps.iter().find(|c| c.cfg_type == VIRTIO_PCI_CAP_PCI_CFG)?;
    
    // Ensure the offset is within the capability's length
    if offset as usize >= cap.length as usize {
        return None;
    }
    
    // PCI CFG capability layout:
    // Offset 0x00: Address register (write target offset here + BAR)
    // Offset 0x04: Data register (read result from here)
    let cfg_offset = cap.offset as usize;
    
    // Write the target offset + BAR to the address register
    // The BAR number goes in the upper 8 bits (bits 24-31)
    let full_value = offset | ((bar as u32) << 24);
    PciConfigSpace::write_config_dword_raw(
        device.bus, device.device, device.function, 
        cfg_offset as u8, 
        full_value
    );
        
    // Read the result from the data register (always 32 bits)
    let val = PciConfigSpace::read_config_dword(
        device.bus, device.device, device.function, 
        (cfg_offset + 4) as u8
    );
    
    // Extract the requested number of bytes based on the offset and width
    let shift = ((offset as usize) & 3) * 8;
    let mask = match width {
        1 => 0xFFu32,
        2 => 0xFFFFu32,
        4 => 0xFFFFFFFFu32,
        _ => return None, // Invalid width
    };
    
    Some((val >> shift) & mask)
}

/// Write a register to the device via PCI Configuration Access Capability (Type 5)
pub fn write_virtio_reg_via_pci_cfg(
    device: &PciDevice,
    bar: u8,
    offset: u32,
    value: u32,
    width: u32,   // 1, 2, or 4 bytes
) -> Option<()> {
    let caps = get_virtio_caps(device);
    let cap = caps.iter().find(|c| c.cfg_type == VIRTIO_PCI_CAP_PCI_CFG)?;
    
    // Ensure the offset is within the capability's length
    if offset as usize >= cap.length as usize {
        return None;
    }
    
    // PCI CFG capability layout:
    // Offset 0x00: Address register (write target offset here + BAR)
    // Offset 0x04: Data register (write value here)
    let cfg_offset = cap.offset as usize;
    
    // Write the target offset + BAR to the address register
    // The BAR number goes in the upper 8 bits (bits 24-31)
    let full_value = offset | ((bar as u32) << 24);
    PciConfigSpace::write_config_dword_raw(
        device.bus, device.device, device.function, 
        cfg_offset as u8, 
        full_value
    );
    
    // Read the current 32-bit value from the data register
    let mut current = PciConfigSpace::read_config_dword(
        device.bus, device.device, device.function, 
        (cfg_offset + 4) as u8
    );
    
    // Calculate shift and mask based on the offset and width
    let shift = ((offset as usize) & 3) * 8;
    let mask = match width {
        1 => 0xFFu32,
        2 => 0xFFFFu32,
        4 => 0xFFFFFFFFu32,
        _ => return None, // Invalid width
    };
    
    // Clear the bits we're about to write, then set them
    let cleared = current & !(mask << shift);
    let masked_value = (value << shift) & mask;
    let new_value = cleared | masked_value;
    
    // Write the modified value back
    PciConfigSpace::write_config_dword_raw(
        device.bus, device.device, device.function, 
        (cfg_offset + 4) as u8, 
        new_value
    );
    Some(())
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