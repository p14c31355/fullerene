/// Bare-metal graphics detection using direct PCI access without UEFI protocols
pub mod bare_metal_graphics_detection {

    use crate::serial::_print;

    macro_rules! log_bm {
        ($($arg:tt)*) => { _print(format_args!($($arg)*)) };
    }

    pub fn detect_bare_metal_graphics() -> Option<crate::common::FullereneFramebufferConfig> {
        log_bm!("[BM-GFX] Starting bare-metal graphics detection...\n");

        let graphics_devices = crate::bare_metal_pci::enumerate_graphics_devices();
        log_bm!(
            "[BM-GFX] Found {} graphics devices via direct PCI enumeration\n",
            graphics_devices.len()
        );

        for device in graphics_devices.iter() {
            log_bm!(
                "[BM-GFX] Probing device {:04x}:{:04x} at {:02x}:{:02x}:{:02x}\n",
                device.vendor_id,
                device.device_id,
                device.bus,
                device.device,
                device.function
            );

            match (device.vendor_id, device.device_id) {
                (0x1af4, id) if id >= 0x1050 => {
                    log_bm!(
                        "[BM-GFX] Detected virtio-gpu, attempting bare-metal framebuffer detection\n"
                    );
                    if let Some(config) = detect_bare_metal_virtio_gpu_framebuffer(device) {
                        log_bm!(
                            "[BM-GFX] Bare-metal virtio-gpu framebuffer detection successful!\n"
                        );
                        return Some(config);
                    }
                }
                (0x1b36, 0x0100) => {
                    log_bm!(
                        "[BM-GFX] Detected QXL device, attempting bare-metal framebuffer detection\n"
                    );
                    if let Some(config) = detect_bare_metal_qxl_framebuffer(device) {
                        log_bm!("[BM-GFX] Bare-metal QXL framebuffer detection successful!\n");
                        return Some(config);
                    }
                }
                (0x1013, _) => {
                    log_bm!(
                        "[BM-GFX] Detected Cirrus Logic VGA device, attempting bare-metal framebuffer detection\n"
                    );
                    if let Some(config) = detect_bare_metal_cirrus_framebuffer(device) {
                        log_bm!(
                            "[BM-GFX] Bare-metal Cirrus VGA framebuffer detection successful!\n"
                        );
                        return Some(config);
                    }
                }
                (0x15ad, 0x0405) => {
                    log_bm!(
                        "[BM-GFX] Detected VMware SVGA, attempting bare-metal framebuffer detection\n"
                    );
                    if let Some(config) = detect_bare_metal_vmware_svga_framebuffer(device) {
                        log_bm!(
                            "[BM-GFX] Bare-metal VMware SVGA framebuffer detection successful!\n"
                        );
                        return Some(config);
                    }
                }
                _ => {
                    log_bm!("[BM-GFX] Unknown graphics device type, skipping\n");
                }
            }
        }

        log_bm!("[BM-GFX] No supported graphics devices found via bare-metal enumeration\n");
        None
    }

    fn detect_bare_metal_virtio_gpu_framebuffer(
        device: &crate::graphics_alternatives::PciDevice,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        let fb_base_addr =
            crate::bare_metal_pci::read_pci_bar(device.bus, device.device, device.function, 0);
        log_bm!("[BM-GFX] virtio-gpu BAR0: {:#x}\n", fb_base_addr);

        if fb_base_addr == 0 {
            log_bm!("[BM-GFX] virtio-gpu BAR0 is zero, invalid\n");
            return None;
        }

        let standard_modes = [
            (1024, 768, 32, fb_base_addr),
            (1280, 720, 32, fb_base_addr),
            (800, 600, 32, fb_base_addr),
            (640, 480, 32, fb_base_addr),
        ];
        if let Some(config) = crate::detect_standard_modes("virtio-gpu", &standard_modes) {
            return Some(config);
        }

        log_bm!("[BM-GFX] Could not determine valid virtio-gpu framebuffer configuration\n");
        None
    }

    fn detect_bare_metal_qxl_framebuffer(
        device: &crate::graphics_alternatives::PciDevice,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        log_bm!("[BM-GFX] QXL bare-metal detection starting\n");

        let fb_base_addr =
            crate::bare_metal_pci::read_pci_bar(device.bus, device.device, device.function, 1);
        log_bm!("[BM-GFX] QXL BAR1: {:#x}\n", fb_base_addr);

        if fb_base_addr == 0 {
            log_bm!("[BM-GFX] QXL BAR1 is zero, invalid\n");
            return None;
        }

        let standard_modes = [
            (1024, 768, 32, fb_base_addr),
            (1280, 720, 32, fb_base_addr),
            (800, 600, 32, fb_base_addr),
            (640, 480, 32, fb_base_addr),
        ];
        if let Some(config) = crate::detect_standard_modes("QXL", &standard_modes) {
            return Some(config);
        }

        log_bm!("[BM-GFX] Could not determine valid QXL framebuffer configuration\n");
        None
    }

    fn detect_bare_metal_cirrus_framebuffer(
        device: &crate::graphics_alternatives::PciDevice,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        log_bm!("[BM-GFX] Cirrus VGA bare-metal detection starting\n");

        let fb_base_addr =
            crate::bare_metal_pci::read_pci_bar(device.bus, device.device, device.function, 0);
        log_bm!("[BM-GFX] Cirrus VGA BAR0: {:#x}\n", fb_base_addr);

        let fb_addr = if fb_base_addr == 0 {
            log_bm!("[BM-GFX] Using standard VGA address for Cirrus\n");
            0xA0000
        } else {
            fb_base_addr
        };

        let standard_modes = [
            (1024, 768, 32, fb_addr),
            (800, 600, 32, fb_addr),
            (1024, 768, 24, fb_addr),
            (800, 600, 24, fb_addr),
        ];
        if let Some(config) = crate::detect_standard_modes("Cirrus VGA", &standard_modes) {
            return Some(config);
        }

        log_bm!("[BM-GFX] Trying standard VGA mode 13h (320x200x8) for Cirrus\n");
        let vga_config = crate::common::memory::create_framebuffer_config(
            0xA0000,
            320,
            200,
            crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
            8,
            320,
        );

        let test_ptr = 0xA0000 as *mut u8;
        unsafe {
            let original = test_ptr.read_volatile();
            test_ptr.write_volatile(0xAB);
            let readback = test_ptr.read_volatile();
            test_ptr.write_volatile(original);
            if readback == 0xAB {
                log_bm!("[BM-GFX] VGA buffer accessible, using mode 13h\n");
                Some(vga_config)
            } else {
                log_bm!("[BM-GFX] VGA buffer not accessible for Cirrus\n");
                None
            }
        }
    }

    fn detect_bare_metal_vmware_svga_framebuffer(
        _device: &crate::graphics_alternatives::PciDevice,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        log_bm!("[BM-GFX] VMware SVGA bare-metal detection not yet implemented\n");
        None
    }
}

pub use bare_metal_graphics_detection::*;
