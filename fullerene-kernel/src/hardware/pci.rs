//! PCI Device Abstraction
//!
//! This module provides PCI device abstraction and configuration space access
//! for unified hardware management.

use crate::*;
use crate::hardware::register_device;
use alloc::vec::Vec;
use spin::Mutex;

/// PCI Configuration Space Header
#[repr(packed)]
#[derive(Debug, Clone, Copy)]
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

impl PciConfigSpace {
    /// Create a new PCI config space header
    pub fn new() -> Self {
        Self {
            vendor_id: 0,
            device_id: 0,
            command: 0,
            status: 0,
            revision_id: 0,
            prog_if: 0,
            subclass: 0,
            class_code: 0,
            cache_line_size: 0,
            latency_timer: 0,
            header_type: 0,
            bist: 0,
        }
    }

    /// Read PCI config space from hardware
    pub fn read_from_device(bus: u8, device: u8, function: u8) -> Option<Self> {
        if !Self::device_exists(bus, device, function) {
            return None;
        }

        let mut config = Self::new();

        // Read vendor and device ID first to verify device exists
        let vendor_id_value = Self::read_config_word(bus, device, function, 0);
        if vendor_id_value == 0xFFFF {
            return None;
        }
        unsafe { core::ptr::write_unaligned(core::ptr::addr_of_mut!(config.vendor_id), vendor_id_value) };

        // Read the rest of the configuration space
        unsafe { core::ptr::write_unaligned(core::ptr::addr_of_mut!(config.device_id), Self::read_config_word(bus, device, function, 2)) };
        unsafe { core::ptr::write_unaligned(core::ptr::addr_of_mut!(config.command), Self::read_config_word(bus, device, function, 4)) };
        unsafe { core::ptr::write_unaligned(core::ptr::addr_of_mut!(config.status), Self::read_config_word(bus, device, function, 6)) };
        unsafe { core::ptr::write_unaligned(core::ptr::addr_of_mut!(config.revision_id), Self::read_config_byte(bus, device, function, 8)) };
        unsafe { core::ptr::write_unaligned(core::ptr::addr_of_mut!(config.prog_if), Self::read_config_byte(bus, device, function, 9)) };
        unsafe { core::ptr::write_unaligned(core::ptr::addr_of_mut!(config.subclass), Self::read_config_byte(bus, device, function, 10)) };
        unsafe { core::ptr::write_unaligned(core::ptr::addr_of_mut!(config.class_code), Self::read_config_byte(bus, device, function, 11)) };
        unsafe { core::ptr::write_unaligned(core::ptr::addr_of_mut!(config.cache_line_size), Self::read_config_byte(bus, device, function, 12)) };
        unsafe { core::ptr::write_unaligned(core::ptr::addr_of_mut!(config.latency_timer), Self::read_config_byte(bus, device, function, 13)) };
        unsafe { core::ptr::write_unaligned(core::ptr::addr_of_mut!(config.header_type), Self::read_config_byte(bus, device, function, 14)) };
        unsafe { core::ptr::write_unaligned(core::ptr::addr_of_mut!(config.bist), Self::read_config_byte(bus, device, function, 15)) };

        Some(config)
    }

    /// Check if a PCI device exists at the given address
    fn device_exists(bus: u8, device: u8, function: u8) -> bool {
        let vendor_id = Self::read_config_word(bus, device, function, 0);
        vendor_id != 0xFFFF
    }

    /// Read a byte from PCI configuration space
    fn read_config_byte(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
        let address = 0x80000000u32
            | ((bus as u32) << 16)
            | ((device as u32) << 11)
            | ((function as u32) << 8)
            | (offset as u32 & 0xFC);

        // Write address to CONFIG_ADDRESS port
        unsafe {
            petroleum::port_write!(
                petroleum::graphics::ports::HardwarePorts::PCI_CONFIG_ADDRESS,
                address
            );
            // Read a single 32-bit dword from the data port
            let mut data_reader: x86_64::instructions::port::Port<u32> =
                x86_64::instructions::port::Port::new(petroleum::graphics::ports::HardwarePorts::PCI_CONFIG_DATA);
            (data_reader.read() & 0xFF) as u8
        }

    }

    /// Read a word from PCI configuration space
    fn read_config_word(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
        let dword = Self::read_config_dword(bus, device, function, offset);
        // The dword is read from the dword-aligned address. We need to select the correct word.
        if offset % 4 < 2 {
            (dword & 0xFFFF) as u16
        } else {
            (dword >> 16) as u16
        }
    }

    /// Read a dword from PCI configuration space
    pub fn read_config_dword(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
        let address = 0x80000000u32
            | ((bus as u32) << 16)
            | ((device as u32) << 11)
            | ((function as u32) << 8)
            | (offset as u32 & 0xFC);

        // Write address to CONFIG_ADDRESS port
        unsafe {
            petroleum::port_write!(
                petroleum::graphics::ports::HardwarePorts::PCI_CONFIG_ADDRESS,
                address
            );
            // Read a single 32-bit dword from the data port
            let mut data_reader: x86_64::instructions::port::Port<u32> =
                x86_64::instructions::port::Port::new(petroleum::graphics::ports::HardwarePorts::PCI_CONFIG_DATA);
            data_reader.read()
        }
    }

    /// Write a byte to PCI configuration space
    pub fn write_config_byte(&mut self, bus: u8, device: u8, function: u8, offset: u8, value: u8) {
        let address = 0x80000000u32
            | ((bus as u32) << 16)
            | ((device as u32) << 11)
            | ((function as u32) << 8)
            | (offset as u32 & 0xFC);

        // Write address to CONFIG_ADDRESS port
        unsafe {
            petroleum::port_write!(
                petroleum::graphics::ports::HardwarePorts::PCI_CONFIG_ADDRESS,
                address
            );
            // Write to the correct byte position within the dword
            let mut data_port = x86_64::instructions::port::Port::<u32>::new(
                petroleum::graphics::ports::HardwarePorts::PCI_CONFIG_DATA
            );
            let current_dword = data_port.read();
            let byte_offset = (offset & 3) as usize;
            let mask = !(0xFFu32 << (byte_offset * 8));
            let new_dword = (current_dword & mask) | ((value as u32) << (byte_offset * 8));
            petroleum::port_write!(
                petroleum::graphics::ports::HardwarePorts::PCI_CONFIG_DATA,
                new_dword
            );
        }
    }

    /// Write a word to PCI configuration space
    pub fn write_config_word(&mut self, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
        // Read the current dword first
        let current_dword = Self::read_config_dword(bus, device, function, offset);

        // Modify the appropriate word within the dword
        let new_dword = if offset % 4 < 2 {
            (current_dword & 0xFFFF0000) | (value as u32)
        } else {
            (current_dword & 0x0000FFFF) | ((value as u32) << 16)
        };

        // Write the address to CONFIG_ADDRESS port (for dword-aligned access)
        let address = 0x80000000u32
            | ((bus as u32) << 16)
            | ((device as u32) << 11)
            | ((function as u32) << 8)
            | (offset as u32 & 0xFC);

        // Write address to CONFIG_ADDRESS port
        unsafe {
            petroleum::port_write!(
                petroleum::graphics::ports::HardwarePorts::PCI_CONFIG_ADDRESS,
                address
            );
            // Write the modified dword to the data port
            petroleum::port_write!(
                petroleum::graphics::ports::HardwarePorts::PCI_CONFIG_DATA,
                new_dword
            );
        }
    }

    /// Write a dword to PCI configuration space
    pub fn write_config_dword(&mut self, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
        let address = 0x80000000u32
            | ((bus as u32) << 16)
            | ((device as u32) << 11)
            | ((function as u32) << 8)
            | (offset as u32 & 0xFC);

        // Write address to CONFIG_ADDRESS port
        unsafe {
            petroleum::port_write!(
                petroleum::graphics::ports::HardwarePorts::PCI_CONFIG_ADDRESS,
                address
            );
            // Write the 32-bit value to the data port
            petroleum::port_write!(
                petroleum::graphics::ports::HardwarePorts::PCI_CONFIG_DATA,
                value
            );
        }
    }
}

/// PCI Device abstraction
pub struct PciDevice {
    bus: u8,
    device: u8,
    function: u8,
    config: PciConfigSpace,
    enabled: bool,
}

impl Clone for PciDevice {
    fn clone(&self) -> Self {
        Self {
            bus: self.bus,
            device: self.device,
            function: self.function,
            config: self.config,
            enabled: self.enabled,
        }
    }
}

impl PciDevice {
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

    /// Get device vendor ID
    pub fn vendor_id(&self) -> u16 {
        // Copy field to avoid unaligned access with packed struct
        unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.config.vendor_id)) }
    }

    /// Get device ID
    pub fn device_id(&self) -> u16 {
        unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.config.device_id)) }
    }

    /// Get device class code
    pub fn class_code(&self) -> u8 {
        unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.config.class_code)) }
    }

    /// Get device subclass
    pub fn subclass(&self) -> u8 {
        unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.config.subclass)) }
    }

    /// Get device revision ID
    pub fn revision_id(&self) -> u8 {
        unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.config.revision_id)) }
    }

    /// Enable memory space access
    pub fn enable_memory_space(&mut self) {
        let command = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.config.command)) } | 0x2; // Enable memory space bit
        self.config.write_config_word(self.bus, self.device, self.function, 4, command);
        unsafe { core::ptr::write_unaligned(core::ptr::addr_of_mut!(self.config.command), command) };
    }

    /// Enable I/O space access
    pub fn enable_io_space(&mut self) {
        let command = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.config.command)) } | 0x1; // Enable I/O space bit
        self.config.write_config_word(self.bus, self.device, self.function, 4, command);
        unsafe { core::ptr::write_unaligned(core::ptr::addr_of_mut!(self.config.command), command) };
    }

    /// Enable bus mastering
    pub fn enable_bus_master(&mut self) {
        let command = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.config.command)) } | 0x4; // Enable bus master bit
        self.config.write_config_word(self.bus, self.device, self.function, 4, command);
        unsafe { core::ptr::write_unaligned(core::ptr::addr_of_mut!(self.config.command), command) };
    }

    /// Get base address register value
    pub fn get_bar(&self, bar_index: usize) -> u32 {
        if bar_index < 6 {
            let offset = 0x10 + (bar_index << 2); // bar_index * 4 shifted left by 2
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

impl Initializable for PciDevice {
    fn init(&mut self) -> SystemResult<()> {
        log_info!("PCI device initialized");
        Ok(())
    }

    fn name(&self) -> &'static str {
        "PciDevice"
    }

    fn priority(&self) -> i32 {
        20 // Medium priority for PCI devices
    }
}

impl ErrorLogging for PciDevice {
    fn log_error(&self, error: &SystemError, context: &'static str) {
        log_error!(error, context);
    }

    fn log_warning(&self, message: &'static str) {
        log_warning!(message);
    }

    fn log_info(&self, message: &'static str) {
        log_info!(message);
    }
}

impl HardwareDevice for PciDevice {
    fn device_name(&self) -> &'static str {
        "PCI Device"
    }

    fn device_type(&self) -> &'static str {
        "PCI"
    }

    fn enable(&mut self) -> SystemResult<()> {
        self.enable_memory_space();
        self.enable_io_space();
        self.enable_bus_master();
        self.enabled = true;
        log_info!("PCI device enabled");
        Ok(())
    }

    fn disable(&mut self) -> SystemResult<()> {
        let command = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(self.config.command)) } & !0x7; // Disable memory, I/O, and bus master bits
        unsafe { core::ptr::write_unaligned(core::ptr::addr_of_mut!(self.config.command), command) };
        self.config.write_config_word(self.bus, self.device, self.function, 4, command);
        self.enabled = false;
        log_info!("PCI device disabled");
        Ok(())
    }

    fn reset(&mut self) -> SystemResult<()> {
        // PCI reset typically requires special handling
        log_info!("PCI device reset");
        Ok(())
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// PCI Bus scanner for device discovery
pub struct PciScanner {
    devices: Vec<PciDevice>,
}

impl PciScanner {
    /// Create a new PCI scanner
    pub fn new() -> Self {
        Self {
            devices: Vec::new(),
        }
    }

    /// Scan all PCI buses for devices
    pub fn scan_all_buses(&mut self) -> SystemResult<()> {
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

        log_info!("PCI scan completed");
        Ok(())
    }

    /// Get all discovered devices
    pub fn get_devices(&self) -> &[PciDevice] {
        &self.devices
    }

    /// Find devices by class code
    pub fn find_devices_by_class(&self, class_code: u8, subclass: u8) -> Vec<&PciDevice> {
        self.devices
            .iter()
            .filter(|device| device.class_code() == class_code && device.subclass() == subclass)
            .collect()
    }

    /// Find devices by vendor ID
    pub fn find_devices_by_vendor(&self, vendor_id: u16) -> Vec<&PciDevice> {
        self.devices
            .iter()
            .filter(|device| device.vendor_id() == vendor_id)
            .collect()
    }
}

impl Initializable for PciScanner {
    fn init(&mut self) -> SystemResult<()> {
        self.scan_all_buses()?;
        log_info!("PCI scanner initialized");
        Ok(())
    }

    fn name(&self) -> &'static str {
        "PciScanner"
    }

    fn priority(&self) -> i32 {
        15 // High priority for PCI scanning
    }
}

impl ErrorLogging for PciScanner {
    fn log_error(&self, error: &SystemError, context: &'static str) {
        log_error!(error, context);
    }

    fn log_warning(&self, message: &'static str) {
        log_warning!(message);
    }

    fn log_info(&self, message: &'static str) {
        log_info!(message);
    }
}

// Global PCI scanner instance
static PCI_SCANNER: Mutex<Option<PciScanner>> = Mutex::new(None);

/// Initialize the global PCI scanner
pub fn init_pci_scanner() -> SystemResult<()> {
    let mut scanner = PCI_SCANNER.lock();
    *scanner = Some(PciScanner::new());
    log_info!("Global PCI scanner initialized");
    Ok(())
}

/// Get a reference to the global PCI scanner
pub fn get_pci_scanner() -> &'static Mutex<Option<PciScanner>> {
    &PCI_SCANNER
}

/// Register all discovered PCI devices with the device manager
pub fn register_pci_devices() -> SystemResult<()> {
    if let Some(scanner) = PCI_SCANNER.lock().as_mut() {
        for device in scanner.get_devices() {
            // Create a boxed copy for registration by cloning
            let pci_device = device.clone(); // Assumes PciDevice implements Clone
            let boxed_device: alloc::boxed::Box<dyn crate::HardwareDevice + Send> =
                alloc::boxed::Box::new(pci_device);
            register_device(boxed_device)?;
        }
    }
    log_info!("PCI devices registered");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pci_config_space_creation() {
        let config = PciConfigSpace::new();
        // Use safe methods to avoid unaligned access with packed struct
        unsafe {
            let vendor_id = core::ptr::read_unaligned(core::ptr::addr_of!(config.vendor_id));
            let device_id = core::ptr::read_unaligned(core::ptr::addr_of!(config.device_id));
            assert_eq!(vendor_id, 0);
            assert_eq!(device_id, 0);
        }
    }

    #[test]
    fn test_pci_device_creation() {
        // Note: This test would require actual PCI hardware to be meaningful
        // For unit testing, we can only test the structure creation
        let config = PciConfigSpace::new();
        // In a real test environment, we would test actual PCI device creation
    }

    #[test]
    fn test_pci_scanner_creation() {
        let scanner = PciScanner::new();
        assert_eq!(scanner.get_devices().len(), 0);
    }
}
