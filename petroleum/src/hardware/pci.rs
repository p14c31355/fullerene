//! PCI Device Abstraction
//!
//! This module provides PCI device abstraction and configuration space access
//! for unified hardware management.

use crate::graphics::ports::PortWriter;

/// PCI Configuration Space Header
#[repr(packed)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PciConfigSpace {
    pub vendor_id: u16,
    pub device_id: u16,
    pub command: u16,
    pub status: u16,
    pub revision_id: u8,
    pub prog_if: u8,
    pub subclass: u8,
    pub class_code: u8,
    pub cache_line_size: u8,
    pub latency_timer: u8,
    pub header_type: u8,
    pub bist: u8,
}

const PCI_CONFIG_ADDR: u16 = 0xCF8;
const PCI_CONFIG_DATA: u16 = 0xCFC;

impl PciConfigSpace {
    /// Create a new PCI config space header
    pub fn new() -> Self {
        Self::default()
    }

    /// Read PCI config space from hardware
    pub fn read_from_device(bus: u8, device: u8, function: u8) -> Option<Self> {
        if !Self::device_exists(bus, device, function) {
            return None;
        }

        let mut config = Self::new();
        config.read_config_space(bus, device, function);
        Some(config)
    }

    /// Check if a PCI device exists at the given address
    fn device_exists(bus: u8, device: u8, function: u8) -> bool {
        Self::read_config_word(bus, device, function, 0) != 0xFFFF
    }

    fn read_config_space(&mut self, bus: u8, device: u8, function: u8) {
        // Read vendor and device ID
        self.vendor_id = Self::read_config_word(bus, device, function, 0);
        self.device_id = Self::read_config_word(bus, device, function, 2);
        self.command = Self::read_config_word(bus, device, function, 4);
        self.status = Self::read_config_word(bus, device, function, 6);

        self.revision_id = Self::read_config_byte(bus, device, function, 8);
        self.prog_if = Self::read_config_byte(bus, device, function, 9);
        self.subclass = Self::read_config_byte(bus, device, function, 10);
        self.class_code = Self::read_config_byte(bus, device, function, 11);
        self.cache_line_size = Self::read_config_byte(bus, device, function, 12);
        self.latency_timer = Self::read_config_byte(bus, device, function, 13);
        self.header_type = Self::read_config_byte(bus, device, function, 14);
        self.bist = Self::read_config_byte(bus, device, function, 15);
    }

    /// Read a byte from PCI configuration space
    fn read_config_byte(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
        let address = Self::build_config_address(bus, device, function, offset);
        let mut addr_writer = PortWriter::new(crate::graphics::HardwarePorts::PCI_CONFIG_ADDRESS);
        let mut data_reader = PortWriter::new(crate::graphics::HardwarePorts::PCI_CONFIG_DATA);

        unsafe {
            addr_writer.write_safe(address);
            let dword: u32 = data_reader.read_safe();
            (dword >> ((offset & 3) * 8)) as u8
        }
    }

    /// Read a word from PCI configuration space
    fn read_config_word(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
        let dword = Self::read_config_dword(bus, device, function, offset);
        let shift = if offset % 4 < 2 { 0 } else { 16 };
        ((dword >> shift) & 0xFFFF) as u16
    }

    /// Read a dword from PCI configuration space
    pub fn read_config_dword(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
        let address = Self::build_config_address(bus, device, function, offset);
        let mut addr_writer = PortWriter::new(crate::graphics::HardwarePorts::PCI_CONFIG_ADDRESS);
        let mut data_reader = PortWriter::new(crate::graphics::HardwarePorts::PCI_CONFIG_DATA);

        unsafe {
            addr_writer.write_safe(address);
            data_reader.read_safe()
        }
    }

    /// Write a byte to PCI configuration space
    pub fn write_config_byte(&mut self, _bus: u8, _device: u8, _function: u8, offset: u8, value: u8) {
        // Write the byte by reading-modifying-writing the entire 32-bit register
        let address = Self::build_config_address(_bus, _device, _function, offset);
        Self::write_config_byte_raw(address, offset, value);

        // Update local copy safely for packed struct
        match offset {
            8 => self.revision_id = value,
            9 => self.prog_if = value,
            10 => self.subclass = value,
            11 => self.class_code = value,
            12 => self.cache_line_size = value,
            13 => self.latency_timer = value,
            14 => self.header_type = value,
            15 => self.bist = value,
            _ => {} // For other offsets, we don't update the struct
        }
    }

    /// Write a word to PCI configuration space
    pub fn write_config_word(&mut self, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
        Self::write_config_dword_raw(bus, device, function, offset, value as u32);

        // Update local copy safely for packed struct
        match offset {
            0 => self.vendor_id = value,
            2 => self.device_id = value,
            4 => self.command = value,
            6 => self.status = value,
            _ => {} // For other offsets, we don't update the struct
        }
    }

    /// Write a dword to PCI configuration space
    pub fn write_config_dword(&mut self, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
        Self::write_config_dword_raw(bus, device, function, offset, value);
    }

    /// Build PCI configuration address
    fn build_config_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
        0x80000000u32 | ((bus as u32) << 16) | ((device as u32) << 11) | ((function as u32) << 8) | (offset as u32 & 0xFC)
    }

    fn write_config_byte_raw(address: u32, offset: u8, value: u8) {
        let mut addr_writer = PortWriter::new(crate::graphics::HardwarePorts::PCI_CONFIG_ADDRESS);
        let mut data_writer = PortWriter::new(crate::graphics::HardwarePorts::PCI_CONFIG_DATA);
        let mut data_reader = PortWriter::new(crate::graphics::HardwarePorts::PCI_CONFIG_DATA);

        unsafe {
            addr_writer.write_safe(address);
            let current_dword: u32 = data_reader.read_safe();
            let byte_offset = (offset & 3) as usize;
            let mask = !(0xFFu32 << (byte_offset * 8));
            let new_dword: u32 = (current_dword & mask) | ((value as u32) << (byte_offset * 8));
            data_writer.write_safe(new_dword);
        }
    }

    fn write_config_dword_raw(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
        let address = Self::build_config_address(bus, device, function, offset);
        let mut addr_writer = PortWriter::new(crate::graphics::HardwarePorts::PCI_CONFIG_ADDRESS);
        let mut data_writer = PortWriter::new(crate::graphics::HardwarePorts::PCI_CONFIG_DATA);

        unsafe {
            addr_writer.write_safe(address);
            data_writer.write_safe(value);
        }
    }
}

/// PCI Device abstraction - public struct for external use
#[derive(Debug, Clone)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub handle: usize,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
}

impl PciDevice {
    /// Create a new PCI device instance
    pub fn new(bus: u8, device: u8, function: u8) -> Option<Self> {
        if let Some(config) = PrivatePciDevice::new(bus, device, function) {
            Some(config.to_public())
        } else {
            None
        }
    }
}

// Legacy PciDevice implementation for kernel use with configuration space management
struct PrivatePciDevice {
    bus: u8,
    device: u8,
    function: u8,
    config: PciConfigSpace,
    enabled: bool,
}

impl PrivatePciDevice {
    /// Create a new PCI device instance
    pub fn new(bus: u8, device: u8, function: u8) -> Option<Self> {
        if let Some(config) = PciConfigSpace::read_from_device(bus, device, function) {
            Some(Self {
                bus,
                device,
                function,
                config,
                enabled: false,
            })
        } else {
            None
        }
    }

    /// Convert to public PciDevice
    pub fn to_public(self) -> PciDevice {
        PciDevice {
            bus: self.bus,
            device: self.device,
            function: self.function,
            handle: Self::build_handle(self.bus, self.device, self.function),
            vendor_id: self.config.vendor_id,
            device_id: self.config.device_id,
            class_code: self.config.class_code,
            subclass: self.config.subclass,
        }
    }

    fn build_handle(bus: u8, device: u8, function: u8) -> usize {
        ((bus as usize) << 16) | ((device as usize) << 8) | (function as usize)
    }

    /// Get device vendor ID
    pub fn vendor_id(&self) -> u16 {
        self.config.vendor_id
    }

    /// Get device ID
    pub fn device_id(&self) -> u16 {
        self.config.device_id
    }

    /// Get device class code
    pub fn class_code(&self) -> u8 {
        self.config.class_code
    }

    /// Get device subclass
    pub fn subclass(&self) -> u8 {
        self.config.subclass
    }

    /// Get device revision ID
    pub fn revision_id(&self) -> u8 {
        self.config.revision_id
    }

    /// Enable memory space access
    pub fn enable_memory_space(&mut self) {
        self.write_command_reg(self.config.command | 0x2);
    }

    /// Enable I/O space access
    pub fn enable_io_space(&mut self) {
        self.write_command_reg(self.config.command | 0x1);
    }

    /// Enable bus mastering
    pub fn enable_bus_master(&mut self) {
        self.write_command_reg(self.config.command | 0x4);
    }

    fn write_command_reg(&mut self, new_command: u16) {
        self.config.write_config_word(self.bus, self.device, self.function, 4, new_command);
        self.config.command = new_command;
    }

    /// Get base address register value
    pub fn get_bar(&self, bar_index: usize) -> u32 {
        if bar_index < 6 {
            let offset = 0x10 + (bar_index << 2);
            PciConfigSpace::read_config_dword(self.bus, self.device, self.function, offset as u8)
        } else {
            0
        }
    }

    /// Set base address register value
    pub fn set_bar(&mut self, bar_index: usize, value: u32) {
        if bar_index < 6 {
            let offset = 0x10 + (bar_index * 4);
            self.config.write_config_dword(self.bus, self.device, self.function, offset as u8, value);
        }
    }
}

/// PCI Bus scanner for device discovery
pub struct PciScanner {
    devices: alloc::vec::Vec<PciDevice>,
}

impl PciScanner {
    /// Create a new PCI scanner
    pub fn new() -> Self {
        Self {
            devices: alloc::vec::Vec::new(),
        }
    }

    /// Scan all PCI buses for devices
    pub fn scan_all_buses(&mut self) -> Result<(), ()> {
        self.devices.clear();

        for bus in 0..=255u8 {
            for device in 0..=31u8 {
                for function in 0..=7u8 {
                    if let Some(pci_device) = PciDevice::new(bus, device, function) {
                        self.devices.push(pci_device);
                    }
                }
            }
        }
        Ok(())
    }

    /// Get all discovered devices
    pub fn get_devices(&self) -> &[PciDevice] {
        &self.devices
    }

    /// Find devices by class code
    pub fn find_devices_by_class(&self, class_code: u8, subclass: u8) -> alloc::vec::Vec<&PciDevice> {
        self.devices
            .iter()
            .filter(|device| device.class_code == class_code && device.subclass == subclass)
            .collect()
    }

    /// Find devices by vendor ID
    pub fn find_devices_by_vendor(&self, vendor_id: u16) -> alloc::vec::Vec<&PciDevice> {
        self.devices
            .iter()
            .filter(|device| device.vendor_id == vendor_id)
            .collect()
    }
}
