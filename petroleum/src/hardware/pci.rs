//! PCI - re-export from nitrogen + policy-level PciAllocator.

use nitrogen::pci::*;

/// Policy-level PCI BAR allocator.
///
/// **NOTE**: This is NOT part of the pure hardware-mechanism layer (`nitrogen`).
/// It lives here as a convenience for kernel consumers and will eventually
/// move into `fullerene-kernel`.
pub struct PciAllocator {
    pub mmio_base: u64,
}

impl PciAllocator {
    pub fn new(mmio_base: u64) -> Self {
        Self { mmio_base }
    }

    pub fn assign_bars(&mut self, devices: &[PciDevice]) {
        // The full implementation iterates BARs, allocates MMIO space, etc.
        // For simplicity we delegate to the original logic from nitrogen's
        // removed PciAllocator — see git history for details.
        for device in devices {
            crate::serial::serial_log(format_args!(
                "[PCI-Allocator] Checking device {:#x}:{:#x} at {}:{}:{}\n",
                device.vendor_id,
                device.device_id,
                device.bus,
                device.device,
                device.function
            ));
            // 1. Disable Memory Space access (Command bit 1)
            let cmd_offset = 4;
            let original_command = PciConfigSpace::read_config_word(
                device.bus,
                device.device,
                device.function,
                cmd_offset,
            );
            crate::serial::serial_log(format_args!(
                "[PCI-Allocator]   original_command={:#x}\n",
                original_command
            ));
            // Use write_config_word_raw to avoid corrupting the Status register (offset 6).
            PciConfigSpace::write_config_word_raw(
                device.bus,
                device.device,
                device.function,
                cmd_offset,
                original_command & !0x2,
            );

            for bar_index in 0..6 {
                crate::serial::serial_log(format_args!(
                    "[PCI-Allocator]   probing BAR {}\n",
                    bar_index
                ));
                if let Some(bar) = device.get_bar_info(bar_index) {
                    crate::serial::serial_log(format_args!(
                        "[PCI-Allocator]   BAR {}: addr={:#x} size={:#x} io={} 64bit={}\n",
                        bar_index, bar.address, bar.size, bar.is_io, bar.is_64bit
                    ));
                    if bar.address == 0 && bar.size > 0 {
                        let aligned_addr =
                            (self.mmio_base + (bar.size as u64 - 1)) & !(bar.size as u64 - 1);

                        let offset = 0x10 + (bar_index * 4);

                        PciConfigSpace::write_config_dword_raw(
                            device.bus,
                            device.device,
                            device.function,
                            offset,
                            aligned_addr as u32,
                        );

                        if bar.is_64bit {
                            PciConfigSpace::write_config_dword_raw(
                                device.bus,
                                device.device,
                                device.function,
                                offset + 4,
                                (aligned_addr >> 32) as u32,
                            );
                        }

                        crate::serial::serial_log(format_args!(
                            "[PCI-Allocator]   Assigned BAR {} to {:#x}\n",
                            bar_index,
                            aligned_addr,
                        ));

                        self.mmio_base = aligned_addr + bar.size as u64;
                    } else {
                        crate::serial::serial_log(format_args!(
                            "[PCI-Allocator]   BAR {} already assigned at {:#x}\n",
                            bar_index,
                            bar.address
                        ));
                    }
                } else {
                    crate::serial::serial_log(format_args!(
                        "[PCI-Allocator]   BAR {} not present\n",
                        bar_index
                    ));
                }
            }

            // 3. Re-enable Memory Space access
            crate::serial::serial_log(format_args!(
                "[PCI-Allocator]   re-enabling memory space, cmd={:#x}\n",
                original_command
            ));
            PciConfigSpace::write_config_word_raw(
                device.bus,
                device.device,
                device.function,
                cmd_offset,
                original_command,
            );
        }
    }
}