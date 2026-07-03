//! PCI Device Abstraction
//!
//! This module provides PCI device abstraction and configuration space access
//! for unified hardware management. No kernel or boot crate dependencies — only
//! `x86_64`, `alloc`, and `log`.

use crate::port::PortWriter;

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
#[repr(C)]
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

    fn build_config_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
        0x80000000u32
            | ((bus as u32) << 16)
            | ((device as u32) << 11)
            | ((function as u32) << 8)
            | (offset as u32 & 0xFC)
    }

    pub fn read_config_byte(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
        let address = Self::build_config_address(bus, device, function, offset);
        let mut addr_writer = PortWriter::new(crate::port::HardwarePorts::PCI_CONFIG_ADDRESS);
        let mut data_reader = PortWriter::new(crate::port::HardwarePorts::PCI_CONFIG_DATA);

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
        let mut addr_writer = PortWriter::new(crate::port::HardwarePorts::PCI_CONFIG_ADDRESS);
        let mut data_reader = PortWriter::new(crate::port::HardwarePorts::PCI_CONFIG_DATA);

        addr_writer.write_safe(address);
        data_reader.read_safe()
    }

    pub fn enable_memory_access(&mut self, bus: u8, device: u8, function: u8) {
        let command = self.command | 0x06;
        Self::write_config_word_raw(bus, device, function, 4, command);
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

    /// Write a raw WORD to PCI configuration space.
    ///
    /// Uses the existing dword at the aligned address, modifies only the
    /// relevant 16-bit half, and writes it back. This avoids corrupting the
    /// other half of the dword (e.g. the Status register when writing Command).
    pub fn write_config_word_raw(bus: u8, device: u8, function: u8, offset: u8, value: u16) {
        let aligned = offset & !3;
        let shift = if offset % 4 < 2 { 0 } else { 16 };
        let existing = Self::read_config_dword(bus, device, function, aligned);
        let masked = existing & !(0xFFFFu32 << shift);
        Self::write_config_dword_raw(
            bus,
            device,
            function,
            aligned,
            masked | ((value as u32) << shift),
        );
    }

    /// Write a raw DWORD to PCI configuration space.
    ///
    /// This is a low-level mechanism. Use `write_config_dword` on `PciConfigSpace`
    /// when you need to update the cached header fields as well.
    pub fn write_config_dword_raw(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
        let address = Self::build_config_address(bus, device, function, offset);
        let mut addr_writer = PortWriter::new(crate::port::HardwarePorts::PCI_CONFIG_ADDRESS);
        let mut data_writer = PortWriter::new(crate::port::HardwarePorts::PCI_CONFIG_DATA);

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
        if let Some(dev) = PrivatePciDevice::new(bus, device, function) {
            Some(dev.to_public())
        } else {
            None
        }
    }

    /// Enable memory-space access and bus-mastering for this device.
    /// The caller should invoke this once after obtaining a `PciDevice`
    /// and before performing MMIO or DMA operations.
    pub fn enable_memory_access(&self) {
        let cmd = PciConfigSpace::read_config_word(self.bus, self.device, self.function, 4);
        PciConfigSpace::write_config_word_raw(self.bus, self.device, self.function, 4, cmd | 0x06);
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

        let size = self.detect_bar_size(index);
        if size == 0 {
            return None;
        }

        let is_io = (value & 0x1) != 0;
        let is_64bit = !is_io && ((value & 0x6) == 0x4);
        let is_prefetchable = !is_io && ((value & 0x8) != 0);

        let mut address = if is_io {
            (value & 0xFFFFFFFC) as u64
        } else {
            (value & 0xFFFFFFF0) as u64
        };

        if is_64bit && index < 5 {
            let high_value =
                PciConfigSpace::read_config_dword(self.bus, self.device, self.function, offset + 4);
            address |= (high_value as u64) << 32;
        }

        Some(PciBar {
            index,
            address,
            size,
            is_io,
            is_64bit,
            is_prefetchable,
        })
    }

    /// Ensure the PCI Power Management capability is set to D0.
    ///
    /// Some BIOS/firmware leave the xHCI controller in D3hot,
    /// which disables the USB PHY and prevents CCS from being set.
    /// This method finds the PM capability (cap ID = 0x01) and
    /// writes 0 to the PMCSR power state field (bits 1:0).
    pub fn ensure_d0(&self) {
        let cap_ptr = PciConfigSpace::read_config_byte(self.bus, self.device, self.function, 0x34);
        if cap_ptr == 0 {
            return;
        }
        let mut off = cap_ptr;
        let mut visited = [false; 256];
        loop {
            if off < 0x40 || off > 0xF8 {
                break;
            }
            // Check for cycles
            if visited[off as usize] {
                log::warn!("PCI: capability list cycle detected at offset {:#x}", off);
                break;
            }
            visited[off as usize] = true;

            let cap_id =
                PciConfigSpace::read_config_byte(self.bus, self.device, self.function, off);
            if cap_id == 0x01 {
                let pmcsr =
                    PciConfigSpace::read_config_word(self.bus, self.device, self.function, off + 4);
                let pstate = pmcsr & 0x3;
                if pstate != 0 {
                    log::info!(
                        "PCI: device {:02x}:{:02x}.{} in D{} → requesting D0",
                        self.bus,
                        self.device,
                        self.function,
                        pstate
                    );
                    PciConfigSpace::write_config_word_raw(
                        self.bus,
                        self.device,
                        self.function,
                        off + 4,
                        pmcsr & !0x3,
                    );
                    for _ in 0..10000 {
                        let cur = PciConfigSpace::read_config_word(
                            self.bus,
                            self.device,
                            self.function,
                            off + 4,
                        );
                        if cur & 0x3 == 0 {
                            break;
                        }
                    }
                }
                return;
            }
            let next =
                PciConfigSpace::read_config_byte(self.bus, self.device, self.function, off + 1);
            if next == 0 || next as usize == off as usize {
                break;
            }
            off = next;
        }
    }

    /// Disable ASPM (Active State Power Management) on the PCIe link.
    ///
    /// When ASPM L1 is enabled, the PCIe link may enter a deep sleep
    /// state.  MMIO access to a device behind a sleeping link can hang
    /// the CPU if the link does not wake correctly.  This method finds
    /// the PCI Express capability (cap ID = 0x10) and clears the ASPM
    /// control bits (bits 1:0) in the Link Control register.
    pub fn disable_pcie_aspm(&self) {
        let cap_ptr = PciConfigSpace::read_config_byte(self.bus, self.device, self.function, 0x34);
        if cap_ptr == 0 {
            return;
        }
        let mut off = cap_ptr;
        let mut visited = [false; 256];
        loop {
            if off < 0x40 || off > 0xFC {
                break;
            }
            if visited[off as usize] {
                log::warn!("PCI: capability list cycle detected at offset {:#x}", off);
                break;
            }
            visited[off as usize] = true;
            let cap_id =
                PciConfigSpace::read_config_byte(self.bus, self.device, self.function, off);
            if cap_id == 0x10 {
                // PCI Express capability found
                // Link Control register is at cap_offset + 0x10
                let lnk_ctrl = PciConfigSpace::read_config_word(
                    self.bus,
                    self.device,
                    self.function,
                    off + 0x10,
                );
                let aspm = lnk_ctrl & 0x3;
                if aspm != 0 {
                    log::info!(
                        "PCI: disabling ASPM on {:02x}:{:02x}.{} (was {})",
                        self.bus,
                        self.device,
                        self.function,
                        aspm
                    );
                    PciConfigSpace::write_config_word_raw(
                        self.bus,
                        self.device,
                        self.function,
                        off + 0x10,
                        lnk_ctrl & !0x3,
                    );
                }
                return;
            }
            let next =
                PciConfigSpace::read_config_byte(self.bus, self.device, self.function, off + 1);
            if next == 0 || next as usize == off as usize {
                break;
            }
            off = next;
        }
    }

    pub fn detect_bar_size(&self, bar_index: u8) -> u32 {
        let offset = 0x10 + (bar_index * 4);
        let original_value =
            PciConfigSpace::read_config_dword(self.bus, self.device, self.function, offset);

        // Disable memory and I/O decoding while probing to avoid address conflicts.
        let cmd = PciConfigSpace::read_config_word(self.bus, self.device, self.function, 4);
        PciConfigSpace::write_config_word_raw(self.bus, self.device, self.function, 4, cmd & !0x3);

        PciConfigSpace::write_config_dword_raw(
            self.bus,
            self.device,
            self.function,
            offset,
            0xFFFFFFFF,
        );
        let size_mask =
            PciConfigSpace::read_config_dword(self.bus, self.device, self.function, offset);

        // Restore BAR value and re-enable decoding
        PciConfigSpace::write_config_dword_raw(
            self.bus,
            self.device,
            self.function,
            offset,
            original_value,
        );
        PciConfigSpace::write_config_word_raw(self.bus, self.device, self.function, 4, cmd);

        if size_mask == 0 || size_mask == 0xFFFFFFFF {
            return 0;
        }

        if (size_mask & 0x1) != 0 {
            // I/O
            !(size_mask & 0xFFFFFFFC) + 1
        } else {
            // Memory
            !(size_mask & 0xFFFFFFF0) + 1
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

        // CRITICAL: On real hardware (InsydeH2O), accessing non-existent PCI
        // buses/devices can cause master aborts → SERR# → system hang or
        // triple fault.  We must detect and skip invalid bus numbers early.

        /// Check if a PCI bus actually exists by probing device 0 function 0.
        /// Returns false if bus is absent (returns 0xFFFF on all reads) or
        /// if probing triggers a master abort that locks up the machine.
        fn bus_exists(bus: u8) -> bool {
            // Try to read vendor ID of device 0, function 0 on this bus.
            // 0xFFFF means no device present (or bus doesn't exist).
            let vendor = PciConfigSpace::read_config_word(bus, 0, 0, 0);
            if vendor == 0xFFFF || vendor == 0x0000 {
                return false;
            }
            true
        }

        /// Check if a specific device/function exists
        fn device_exists(bus: u8, device: u8, function: u8) -> bool {
            let vendor = PciConfigSpace::read_config_word(bus, device, function, 0);
            vendor != 0xFFFF && vendor != 0x0000
        }

        // BFS-like bus scan: start with bus 0, then scan child buses found
        // on PCI-to-PCI bridges.  This avoids probing non-existent buses
        // that can cause hangs on real hardware.
        let mut buses_to_scan: [bool; 256] = [false; 256];
        buses_to_scan[0] = true; // Always scan bus 0

        // First pass: scan bus 0 to discover PCI-to-PCI bridges
        for device in 0..=31u8 {
            if !device_exists(0, device, 0) {
                continue;
            }
            for function in 0..=7u8 {
                if function > 0 {
                    let header_type_fn0 = PciConfigSpace::read_config_byte(0, device, 0, 0x0E);
                    if (header_type_fn0 & 0x80) == 0 {
                        // Multi-function bit not set; skip functions 1-7
                        break;
                    }
                }
                if !device_exists(0, device, function) {
                    continue;
                }
                if let Some(pci_device) = PciDevice::new(0, device, function) {
                    // Check if this is a PCI-to-PCI bridge (class 0x06, subclass 0x04)
                    let _header_type = PciConfigSpace::read_config_byte(0, device, function, 0x0E);
                    let class = PciConfigSpace::read_config_byte(0, device, function, 0x0B);
                    let subclass = PciConfigSpace::read_config_byte(0, device, function, 0x0A);

                    self.devices.push(pci_device);

                    if class == 0x06 && subclass == 0x04 {
                        // PCI-to-PCI bridge: read secondary bus number
                        let secondary_bus =
                            PciConfigSpace::read_config_byte(0, device, function, 0x19);
                        if secondary_bus > 0 && secondary_bus < 255 {
                            buses_to_scan[secondary_bus as usize] = true;
                        }
                    }
                }
            }
        }

        // Second pass: scan discovered child buses
        for bus in 1..=255u8 {
            if !buses_to_scan[bus as usize] {
                continue;
            }
            // Verify the bus actually exists before scanning
            if !bus_exists(bus) {
                buses_to_scan[bus as usize] = false;
                continue;
            }
            for device in 0..=31u8 {
                if !device_exists(bus, device, 0) {
                    continue;
                }
                for function in 0..=7u8 {
                    if function > 0 {
                        let header_type = PciConfigSpace::read_config_byte(bus, device, 0, 0x0E);
                        if (header_type & 0x80) == 0 {
                            break;
                        }
                    }
                    if !device_exists(bus, device, function) {
                        continue;
                    }
                    if let Some(pci_device) = PciDevice::new(bus, device, function) {
                        // Check for nested PCI bridges
                        let class = PciConfigSpace::read_config_byte(bus, device, function, 0x0B);
                        let subclass =
                            PciConfigSpace::read_config_byte(bus, device, function, 0x0A);

                        if class == 0x06 && subclass == 0x04 {
                            let secondary_bus =
                                PciConfigSpace::read_config_byte(bus, device, function, 0x19);
                            if secondary_bus > bus && !buses_to_scan[secondary_bus as usize] {
                                buses_to_scan[secondary_bus as usize] = true;
                            }
                        }
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
