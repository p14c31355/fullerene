//! PCI Device Abstraction
//!
//! This module provides PCI device abstraction and configuration space access
//! for unified hardware management.

use crate::hardware::ports::PortWriter;

#[derive(Debug, Clone, Copy)]
pub struct PciBar {
    pub index: u8,
    pub address: u64,
    pub size: u32,
    pub is_io: bool,
    pub is_64bit: bool,
    pub is_prefetchable: bool,
}

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

impl PciConfigSpace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn read_from_device(bus: u8, device: u8, function: u8) -> Option<Self> {
        if !Self::device_exists(bus, device, function) {
            return None;
        }

        let mut config = Self::new();
        config.read_config_space(bus, device, function);
        Some(config)
    }

    fn device_exists(bus: u8, device: u8, function: u8) -> bool {
        Self::read_config_word(bus, device, function, 0) != 0xFFFF
    }

    fn read_config_space(&mut self, bus: u8, device: u8, function: u8) {
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

    pub fn read_config_byte(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
        let address = Self::build_config_address(bus, device, function, offset);
        let mut addr_writer =
            PortWriter::new(crate::hardware::ports::HardwarePorts::PCI_CONFIG_ADDRESS);
        let mut data_reader =
            PortWriter::new(crate::hardware::ports::HardwarePorts::PCI_CONFIG_DATA);

        addr_writer.write_safe(address);
        let dword: u32 = data_reader.read_safe();
        (dword >> ((offset & 3) * 8)) as u8
    }

    pub fn read_config_word(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
        let dword = Self::read_config_dword(bus, device, function, offset);
        let shift = if offset % 4 < 2 { 0 } else { 16 };
        ((dword >> shift) & 0xFFFF) as u16
    }

    pub fn read_config_dword(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
        let address = Self::build_config_address(bus, device, function, offset);
        let mut addr_writer =
            PortWriter::new(crate::hardware::ports::HardwarePorts::PCI_CONFIG_ADDRESS);
        let mut data_reader =
            PortWriter::new(crate::hardware::ports::HardwarePorts::PCI_CONFIG_DATA);

        addr_writer.write_safe(address);
        data_reader.read_safe()
    }

    pub fn enable_memory_access(&mut self, bus: u8, device: u8, function: u8) {
        let command = self.command | 0x06;
        self.write_config_dword(bus, device, function, 4, command as u32);
        self.command = command;
    }

    pub fn write_config_dword(
        &mut self,
        bus: u8,
        device: u8,
        function: u8,
        offset: u8,
        value: u32,
    ) {
        Self::write_config_dword_raw(bus, device, function, offset, value);
    }

    fn build_config_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
        0x80000000u32
            | ((bus as u32) << 16)
            | ((device as u32) << 11)
            | ((function as u32) << 8)
            | (offset as u32 & 0xFC)
    }

    pub(crate) fn write_config_dword_raw(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
        let address = Self::build_config_address(bus, device, function, offset);
        let mut addr_writer =
            PortWriter::new(crate::hardware::ports::HardwarePorts::PCI_CONFIG_ADDRESS);
        let mut data_writer =
            PortWriter::new(crate::hardware::ports::HardwarePorts::PCI_CONFIG_DATA);

        addr_writer.write_safe(address);
        data_writer.write_safe(value);
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
    pub fn new(bus: u8, device: u8, function: u8) -> Option<Self> {
        if let Some(mut dev) = PrivatePciDevice::new(bus, device, function) {
            dev.config.enable_memory_access(bus, device, function);
            Some(dev.to_public())
        } else {
            None
        }
    }

    pub fn read_bar(&self, bar_index: u8) -> Option<u64> {
        if let Some(dev) = PrivatePciDevice::new(self.bus, self.device, self.function) {
            dev.read_bar(bar_index)
        } else {
            None
        }
    }

    pub fn get_bar_info(&self, index: u8) -> Option<PciBar> {
        let offset = 0x10 + (index * 4);
        let value = PciConfigSpace::read_config_dword(self.bus, self.device, self.function, offset);

        if value == 0 {
            return None;
        }

        let is_io = (value & 0x1) != 0;
        let is_64bit = !is_io && ((value & 0x6) == 0x4);
        let is_prefetchable = !is_io && ((value & 0x8) != 0);

        let size = self.detect_bar_size(index);
        let address = if is_io {
            (value & 0xFFFFFFFC) as u64
        } else {
            (value & 0xFFFFFFF0) as u64
        };

        Some(PciBar {
            index,
            address,
            size,
            is_io,
            is_64bit,
            is_prefetchable,
        })
    }

    pub fn detect_bar_size(&self, bar_index: u8) -> u32 {
        let offset = 0x10 + (bar_index * 4);
        let original_value =
            PciConfigSpace::read_config_dword(self.bus, self.device, self.function, offset);

        PciConfigSpace::write_config_dword_raw(
            self.bus,
            self.device,
            self.function,
            offset,
            0xFFFFFFFF,
        );
        let size_mask =
            PciConfigSpace::read_config_dword(self.bus, self.device, self.function, offset);

        // Restore original
        PciConfigSpace::write_config_dword_raw(
            self.bus,
            self.device,
            self.function,
            offset,
            original_value,
        );

        if (size_mask & 0x1) != 0 {
            // I/O
            !(size_mask & 0xFFFFFFFC) + 1
        } else {
            // Memory
            !(size_mask & 0xFFFFFFF0) + 1
        }
    }
}

pub struct PciAllocator {
    pub mmio_base: u64,
}

impl PciAllocator {
    pub fn new(mmio_base: u64) -> Self {
        Self { mmio_base }
    }

    pub fn assign_bars(&mut self, scanner: &PciScanner) {
        for device in scanner.get_devices() {
            crate::serial::_print(format_args!(
                "[PCI-Allocator] Checking device {:#x}:{:#x} at {}:{}:{}\n",
                device.vendor_id, device.device_id, device.bus, device.device, device.function
            ));
            // 1. Disable Memory Space access (Command bit 1)
            let cmd_offset = 4;
            let original_command = PciConfigSpace::read_config_word(
                device.bus,
                device.device,
                device.function,
                cmd_offset,
            );
            PciConfigSpace::write_config_dword_raw(
                device.bus,
                device.device,
                device.function,
                cmd_offset,
                (original_command & !0x2) as u32,
            );

            for bar_index in 0..6 {
                if let Some(mut bar) = device.get_bar_info(bar_index) {
                    if bar.address == 0 && bar.size > 0 {
                        let aligned_addr =
                            (self.mmio_base + (bar.size as u64 - 1)) & !(bar.size as u64 - 1);

                        let offset = 0x10 + (bar_index * 4);

                        // Write low 32 bits
                        PciConfigSpace::write_config_dword_raw(
                            device.bus,
                            device.device,
                            device.function,
                            offset,
                            aligned_addr as u32,
                        );

                        // If 64-bit, write high 32 bits
                        if bar.is_64bit {
                            PciConfigSpace::write_config_dword_raw(
                                device.bus,
                                device.device,
                                device.function,
                                offset + 4,
                                (aligned_addr >> 32) as u32,
                            );
                        }

                        crate::serial::_print(format_args!(
                            "[PCI-Allocator] Assigned BAR {} to {:#x} (size={:#x}, 64bit={})\n",
                            bar_index, aligned_addr, bar.size, bar.is_64bit
                        ));

                        self.mmio_base = aligned_addr + bar.size as u64;
                    } else {
                        crate::serial::_print(format_args!(
                            "[PCI-Allocator] BAR {} is already assigned at {:#x}\n",
                            bar_index, bar.address
                        ));
                    }
                }
            }

            // 3. Re-enable Memory Space access
            PciConfigSpace::write_config_dword_raw(
                device.bus,
                device.device,
                device.function,
                cmd_offset,
                original_command as u32,
            );
        }
    }
}

struct PrivatePciDevice {
    bus: u8,
    device: u8,
    function: u8,
    config: PciConfigSpace,
}

impl PrivatePciDevice {
    pub fn new(bus: u8, device: u8, function: u8) -> Option<Self> {
        if let Some(config) = PciConfigSpace::read_from_device(bus, device, function) {
            Some(Self {
                bus,
                device,
                function,
                config,
            })
        } else {
            None
        }
    }

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

    pub fn read_bar(&self, bar_index: u8) -> Option<u64> {
        if bar_index > 5 {
            return None;
        }

        let offset = 0x10 + (bar_index * 4);
        let bar_low =
            PciConfigSpace::read_config_dword(self.bus, self.device, self.function, offset);

        if bar_low == 0 {
            return None;
        }

        if (bar_low & 0x1) != 0 {
            return None;
        }

        let is_64bit = (bar_low & 0x6) == 0x4;

        if is_64bit {
            if bar_index >= 5 {
                return None;
            }
            let high_offset = offset + 4;
            let bar_high = PciConfigSpace::read_config_dword(
                self.bus,
                self.device,
                self.function,
                high_offset,
            );
            Some(((bar_high as u64) << 32) | ((bar_low & 0xFFFFFFF0) as u64))
        } else {
            Some((bar_low & 0xFFFFFFF0) as u64)
        }
    }

    fn build_handle(bus: u8, device: u8, function: u8) -> usize {
        ((bus as usize) << 16) | ((device as usize) << 8) | (function as usize)
    }
}

pub struct PciScanner {
    devices: alloc::vec::Vec<PciDevice>,
}

impl PciScanner {
    pub fn new() -> Self {
        Self {
            devices: alloc::vec::Vec::new(),
        }
    }

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

    pub fn get_devices(&self) -> &[PciDevice] {
        &self.devices
    }
}