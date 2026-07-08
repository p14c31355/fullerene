use nitrogen::pci::{PciConfigSpace, PciDevice};

/// Policy-level PCI BAR allocator.
pub struct PciAllocator {
    pub mmio_base: u64,
}

impl PciAllocator {
    pub fn new(mmio_base: u64) -> Self {
        Self { mmio_base }
    }

    pub fn assign_bars(&mut self, devices: &[PciDevice]) {
        for device in devices {
            log::info!(
                "[PCI-Allocator] Checking device {:#x}:{:#x} at {}:{}:{}",
                device.vendor_id, device.device_id, device.bus, device.device, device.function
            );
            let cmd_offset = 4;
            let original_command = PciConfigSpace::read_config_word(
                device.bus, device.device, device.function, cmd_offset,
            );
            PciConfigSpace::write_config_word_raw(
                device.bus, device.device, device.function,
                cmd_offset, original_command & !0x2,
            );

            let mut bar_index = 0;
            while bar_index < 6 {
                let advance = if let Some(bar) = device.get_bar_info(bar_index) {
                    let step = if bar.is_64bit { 2 } else { 1 };
                    log::info!(
                        "[PCI-Allocator] BAR {}: addr={:#x} size={:#x} io={} 64bit={}",
                        bar_index, bar.address, bar.size, bar.is_io, bar.is_64bit
                    );
                    if bar.is_io {
                        log::info!("[PCI-Allocator] BAR {} is I/O space; leaving unchanged", bar_index);
                        bar_index += step;
                        continue;
                    }
                    if bar.address == 0 && bar.size > 0 {
                        let aligned_addr = (self.mmio_base + (bar.size as u64 - 1)) & !(bar.size as u64 - 1);
                        let offset = 0x10 + (bar_index * 4);
                        PciConfigSpace::write_config_dword_raw(
                            device.bus, device.device, device.function,
                            offset, aligned_addr as u32,
                        );
                        if bar.is_64bit {
                            PciConfigSpace::write_config_dword_raw(
                                device.bus, device.device, device.function,
                                offset + 4, (aligned_addr >> 32) as u32,
                            );
                        }
                        log::info!("[PCI-Allocator] Assigned BAR {} to {:#x}", bar_index, aligned_addr);
                        self.mmio_base = aligned_addr + bar.size as u64;
                    } else {
                        log::info!("[PCI-Allocator] BAR {} already assigned at {:#x}", bar_index, bar.address);
                    }
                    step
                } else {
                    log::info!("[PCI-Allocator] BAR {} not present", bar_index);
                    1
                };
                bar_index += advance;
            }
            PciConfigSpace::write_config_word_raw(
                device.bus, device.device, device.function,
                cmd_offset, original_command,
            );
        }
    }
}
