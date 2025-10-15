//! Hardware Port I/O Operations
//!
//! This module provides enhanced port I/O operations and hardware port management,
//! centralizing common hardware interfacing patterns to reduce code duplication.

use crate::graphics::ports::HardwarePorts;

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
    pub fn write_register(&mut self, index: u8, value: u8) -> Result<(), ()> {
        crate::port_write!(self.index_port, index);
        crate::port_write!(self.data_port, value);
        Ok(())
    }

    /// Write multiple register values
    pub fn write_registers(&mut self, registers: &[(u8, u8)]) -> Result<(), ()> {
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
    pub fn write_vga_crtc(index: u8, value: u8) -> Result<(), ()> {
        let mut writer =
            VgaRegisterWriter::new(HardwarePorts::CRTC_INDEX, HardwarePorts::CRTC_DATA);
        writer.write_register(index, value)
    }

    /// Write to VGA graphics register
    pub fn write_vga_graphics(index: u8, value: u8) -> Result<(), ()> {
        let mut writer =
            VgaRegisterWriter::new(HardwarePorts::GRAPHICS_INDEX, HardwarePorts::GRAPHICS_DATA);
        writer.write_register(index, value)
    }

    /// Write to VGA sequencer register
    pub fn write_vga_sequencer(index: u8, value: u8) -> Result<(), ()> {
        let mut writer = VgaRegisterWriter::new(
            HardwarePorts::SEQUENCER_INDEX,
            HardwarePorts::SEQUENCER_DATA,
        );
        writer.write_register(index, value)
    }
}

/// PCI configuration space convenience functions
pub mod pci {
    use super::*;

    /// Read PCI configuration byte
    pub fn read_config_byte(offset: u16) -> Result<u8, ()> {
        Ok(crate::port_read_u8!(
            HardwarePorts::PCI_CONFIG_DATA + offset
        ))
    }

    /// Write PCI configuration byte
    pub fn write_config_byte(offset: u16, value: u8) -> Result<(), ()> {
        crate::port_write!(HardwarePorts::PCI_CONFIG_DATA + offset, value);
        Ok(())
    }

    /// Write PCI configuration address
    pub fn write_config_address(address: u32) -> Result<(), ()> {
        crate::port_write!(HardwarePorts::PCI_CONFIG_ADDRESS, address);
        Ok(())
    }
}
