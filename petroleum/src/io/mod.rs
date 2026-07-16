//! Port I/O primitives for early boot and hardware access.
//!
//! Provides safe wrappers around x86 port I/O instructions used by
//! both the boot crate (`petroleum`) and the driver layer (`nitrogen`).

use crate::{port_read_u8, port_write};

/// Generic port I/O wrapper that reduces unsafe repetition.
pub struct PortWriter<T> {
    port: x86_64::instructions::port::Port<T>,
}

impl<T> PortWriter<T> {
    pub fn new(port_addr: u16) -> Self {
        Self {
            port: x86_64::instructions::port::Port::new(port_addr),
        }
    }

    pub fn write_safe(&mut self, value: T)
    where
        T: Copy + x86_64::instructions::port::PortWrite,
    {
        unsafe {
            self.port.write(value);
        }
    }

    pub fn read_safe(&mut self) -> T
    where
        T: x86_64::instructions::port::PortRead,
    {
        unsafe { self.port.read() }
    }
}

/// Generic helper for processing I/O port sequences.
pub trait PortOperations {
    fn write_sequence_u8(&mut self, index_port: u16, data_port: u16, configs: &[(u8, u8)]);
}

impl PortOperations for () {
    fn write_sequence_u8(&mut self, index_port: u16, data_port: u16, configs: &[(u8, u8)]) {
        let mut idx = PortWriter::new(index_port);
        let mut dat = PortWriter::new(data_port);
        for &(index, value) in configs {
            idx.write_safe(index);
            dat.write_safe(value);
        }
    }
}

pub fn write_vga_attribute_register(index: u8, value: u8) {
    port_read_u8!(0x3DA);
    port_write!(0x3C0, index);
    port_write!(0x3C0, value);
}

/// Generic port sequence writer.
pub trait PortSequenceWriter<T> {
    fn write_sequence(&mut self, values: &[T]);
}

impl<T: Copy + x86_64::instructions::port::PortWrite> PortSequenceWriter<T>
    for x86_64::instructions::port::Port<T>
{
    fn write_sequence(&mut self, values: &[T]) {
        for &value in values {
            unsafe { self.write(value) };
        }
    }
}

/// MSR (Model-Specific Register) operations wrapper.
pub struct MsrHelper {
    index: u32,
}

impl MsrHelper {
    pub fn new(index: u32) -> Self {
        Self { index }
    }

    pub fn read(&self) -> u64 {
        unsafe { x86_64::registers::model_specific::Msr::new(self.index).read() }
    }

    pub fn write(&self, value: u64) {
        unsafe { x86_64::registers::model_specific::Msr::new(self.index).write(value) }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RegisterConfig {
    pub index: u8,
    pub value: u8,
}

/// Specialized VGA port operations.
pub struct VgaPortOps {
    index_writer: PortWriter<u8>,
    data_writer: PortWriter<u8>,
}

impl VgaPortOps {
    pub fn new(index_port: u16, data_port: u16) -> Self {
        Self {
            index_writer: PortWriter::new(index_port),
            data_writer: PortWriter::new(data_port),
        }
    }

    pub fn write_register(&mut self, index: u8, value: u8) {
        self.index_writer.write_safe(index);
        self.data_writer.write_safe(value);
    }

    pub fn write_sequence(&mut self, configs: &[RegisterConfig]) {
        for reg in configs {
            self.write_register(reg.index, reg.value);
        }
    }
}

/// Hardware port address constants.
pub struct HardwarePorts;

impl HardwarePorts {
    pub const MISC_OUTPUT: u16 = 0x3C2;
    pub const CRTC_INDEX: u16 = 0x3D4;
    pub const CRTC_DATA: u16 = 0x3D5;
    pub const STATUS: u16 = 0x3DA;
    pub const ATTRIBUTE_INDEX: u16 = 0x3C0;
    pub const DAC_INDEX: u16 = 0x3C8;
    pub const DAC_DATA: u16 = 0x3C9;
    pub const GRAPHICS_INDEX: u16 = 0x3CE;
    pub const GRAPHICS_DATA: u16 = 0x3CF;
    pub const SEQUENCER_INDEX: u16 = 0x3C4;
    pub const SEQUENCER_DATA: u16 = 0x3C5;
    pub const PCI_CONFIG_ADDRESS: u16 = 0xCF8;
    pub const PCI_CONFIG_DATA: u16 = 0xCFC;
    pub const CURSOR_POS_LOW_REG: u8 = 0x0F;
    pub const CURSOR_POS_HIGH_REG: u8 = 0x0E;
    pub const SERIAL_DATA_PORT: u16 = 0x3F8;
    pub const SERIAL_LINE_STATUS_PORT: u16 = 0x3FD;
}

/// VGA register writer for efficient VGA operations.
pub struct VgaRegisterWriter {
    index_port: u16,
    data_port: u16,
}

impl VgaRegisterWriter {
    pub const fn new(index_port: u16, data_port: u16) -> Self {
        Self {
            index_port,
            data_port,
        }
    }

    pub fn write_register(&mut self, index: u8, value: u8) -> Result<(), ()> {
        port_write!(self.index_port, index);
        port_write!(self.data_port, value);
        Ok(())
    }

    pub fn write_registers(&mut self, registers: &[(u8, u8)]) -> Result<(), ()> {
        for &(index, value) in registers {
            self.write_register(index, value)?;
        }
        Ok(())
    }
}

/// Convenience functions for common port operations.
pub mod convenience {
    use super::*;

    pub fn write_vga_crtc(index: u8, value: u8) -> Result<(), ()> {
        VgaRegisterWriter::new(HardwarePorts::CRTC_INDEX, HardwarePorts::CRTC_DATA)
            .write_register(index, value)
    }

    pub fn write_vga_graphics(index: u8, value: u8) -> Result<(), ()> {
        VgaRegisterWriter::new(HardwarePorts::GRAPHICS_INDEX, HardwarePorts::GRAPHICS_DATA)
            .write_register(index, value)
    }

    pub fn write_vga_sequencer(index: u8, value: u8) -> Result<(), ()> {
        VgaRegisterWriter::new(
            HardwarePorts::SEQUENCER_INDEX,
            HardwarePorts::SEQUENCER_DATA,
        )
        .write_register(index, value)
    }
}

/// PCI configuration space convenience functions.
pub mod pci {
    use super::*;

    pub fn read_config_byte(offset: u16) -> Result<u8, ()> {
        Ok(port_read_u8!(HardwarePorts::PCI_CONFIG_DATA + offset))
    }

    pub fn write_config_byte(offset: u16, value: u8) -> Result<(), ()> {
        port_write!(HardwarePorts::PCI_CONFIG_DATA + offset, value);
        Ok(())
    }

    pub fn write_config_address(address: u32) -> Result<(), ()> {
        port_write!(HardwarePorts::PCI_CONFIG_ADDRESS, address);
        Ok(())
    }
}

/// Safe port write macro.
#[macro_export]
macro_rules! port_write {
    ($port_addr:expr, $value:expr) => {{
        let mut writer = $crate::io::PortWriter::new($port_addr);
        writer.write_safe($value);
    }};
}

/// Safe port read macro.
#[macro_export]
macro_rules! port_read_u8 {
    ($port_addr:expr) => {{
        let mut reader: $crate::io::PortWriter<u8> = $crate::io::PortWriter::new($port_addr);
        reader.read_safe()
    }};
}

#[macro_export]
macro_rules! port_read {
    ($port_addr:expr) => {
        port_read_u8!($port_addr)
    };
}

/// Enhanced macro for writing port sequences.
#[macro_export]
macro_rules! write_port_sequence {
    ($($config:expr, $index_port:expr, $data_port:expr);*$(;)?) => {{
        $(
            let mut vga_ports = $crate::io::VgaPortOps::new($index_port, $data_port);
            vga_ports.write_sequence($config);
        )*
    }};
}

/// Simplified macro for single register writes.
#[macro_export]
macro_rules! write_vga_register {
    ($index_port:expr, $data_port:expr, $index:expr, $data:expr) => {{
        let mut vga_ports = $crate::io::VgaPortOps::new($index_port, $data_port);
        vga_ports.write_register($index, $data);
    }};
}
