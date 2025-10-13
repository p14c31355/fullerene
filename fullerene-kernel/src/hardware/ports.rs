//! Enhanced Port Operations
//!
//! This module provides improved port operations with unified interface,
//! reducing unsafe code usage and improving type safety.

use crate::*;

/// Hardware port addresses for common hardware interfaces
pub struct HardwarePorts;

impl HardwarePorts {
    /// VGA port addresses
    pub const VGA_MISC_OUTPUT: u16 = 0x3C2;
    pub const VGA_CRTC_INDEX: u16 = 0x3D4;
    pub const VGA_CRTC_DATA: u16 = 0x3D5;
    pub const VGA_STATUS: u16 = 0x3DA;
    pub const VGA_ATTRIBUTE_INDEX: u16 = 0x3C0;
    pub const VGA_DAC_INDEX: u16 = 0x3C8;
    pub const VGA_DAC_DATA: u16 = 0x3C9;
    pub const VGA_GRAPHICS_INDEX: u16 = 0x3CE;
    pub const VGA_GRAPHICS_DATA: u16 = 0x3CF;
    pub const VGA_SEQUENCER_INDEX: u16 = 0x3C4;
    pub const VGA_SEQUENCER_DATA: u16 = 0x3C5;

    /// PCI port addresses
    pub const PCI_CONFIG_ADDRESS: u16 = 0xCF8;
    pub const PCI_CONFIG_DATA: u16 = 0xCFC;

    /// PIC port addresses
    pub const PIC1_COMMAND: u16 = 0x20;
    pub const PIC1_DATA: u16 = 0x21;
    pub const PIC2_COMMAND: u16 = 0xA0;
    pub const PIC2_DATA: u16 = 0xA1;

    /// PIT port addresses
    pub const PIT_CHANNEL0: u16 = 0x40;
    pub const PIT_CHANNEL1: u16 = 0x41;
    pub const PIT_CHANNEL2: u16 = 0x42;
    pub const PIT_COMMAND: u16 = 0x43;

    /// PS/2 port addresses
    pub const PS2_DATA: u16 = 0x60;
    pub const PS2_STATUS_COMMAND: u16 = 0x64;

    /// Serial port addresses
    pub const SERIAL_COM1: u16 = 0x3F8;
    pub const SERIAL_COM2: u16 = 0x2F8;
    pub const SERIAL_COM3: u16 = 0x3E8;
    pub const SERIAL_COM4: u16 = 0x2E8;

    /// ATA port addresses
    pub const ATA_PRIMARY_COMMAND: u16 = 0x1F0;
    pub const ATA_PRIMARY_CONTROL: u16 = 0x3F6;
    pub const ATA_SECONDARY_COMMAND: u16 = 0x170;
    pub const ATA_SECONDARY_CONTROL: u16 = 0x376;
}

/// VGA register writer for efficient VGA operations
pub struct VgaRegisterWriter {
    index_port: u16,
    data_port: u16,
}

impl VgaRegisterWriter {
    /// Create a new VGA register writer
    pub const fn new(index_port: u16, data_port: u16) -> Self {
        Self {
            index_port,
            data_port,
        }
    }

    /// Write a register value
    pub fn write_register(&mut self, index: u8, value: u8) -> SystemResult<()> {
        unsafe {
            petroleum::port_write!(self.index_port, index);
            petroleum::port_write!(self.data_port, value);
        }
        Ok(())
    }

    /// Write multiple register values
    pub fn write_registers(&mut self, registers: &[(u8, u8)]) -> SystemResult<()> {
        for &(index, value) in registers {
            self.write_register(index, value)?;
        }
        Ok(())
    }
}

/// Convenience functions for common port operations
pub mod convenience {
    use super::*;

    /// Write to VGA CRTC register
    pub fn write_vga_crtc(index: u8, value: u8) -> SystemResult<()> {
        let mut writer = VgaRegisterWriter::new(
            HardwarePorts::VGA_CRTC_INDEX,
            HardwarePorts::VGA_CRTC_DATA,
        );
        writer.write_register(index, value)
    }

    /// Write to VGA sequencer register
    pub fn write_vga_sequencer(index: u8, value: u8) -> SystemResult<()> {
        let mut writer = VgaRegisterWriter::new(
            HardwarePorts::VGA_SEQUENCER_INDEX,
            HardwarePorts::VGA_SEQUENCER_DATA,
        );
        writer.write_register(index, value)
    }

    /// Write to VGA graphics register
    pub fn write_vga_graphics(index: u8, value: u8) -> SystemResult<()> {
        let mut writer = VgaRegisterWriter::new(
            HardwarePorts::VGA_GRAPHICS_INDEX,
            HardwarePorts::VGA_GRAPHICS_DATA,
        );
        writer.write_register(index, value)
    }

    /// Write PCI configuration address
    pub fn write_pci_config_address(address: u32) -> SystemResult<()> {
        unsafe { petroleum::port_write!(HardwarePorts::PCI_CONFIG_ADDRESS, address); }
        Ok(())
    }

    /// Read PCI configuration data byte
    pub fn read_pci_config_byte(offset: u16) -> SystemResult<u8> {
        Ok(unsafe { petroleum::port_read_u8!(HardwarePorts::PCI_CONFIG_DATA + offset) })
    }

    /// Write PCI configuration data byte
    pub fn write_pci_config_byte(offset: u16, value: u8) -> SystemResult<()> {
        unsafe { petroleum::port_write!(HardwarePorts::PCI_CONFIG_DATA + offset, value); }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vga_register_writer_creation() {
        let writer = VgaRegisterWriter::new(0x3D4, 0x3D5);
        // Note: We can't actually test register writing in unit tests
        // as it requires hardware access
    }

    #[test]
    fn test_hardware_ports_constants() {
        assert_eq!(HardwarePorts::VGA_CRTC_INDEX, 0x3D4);
        assert_eq!(HardwarePorts::VGA_CRTC_DATA, 0x3D5);
        assert_eq!(HardwarePorts::PCI_CONFIG_ADDRESS, 0xCF8);
        assert_eq!(HardwarePorts::PCI_CONFIG_DATA, 0xCFC);
    }
}
