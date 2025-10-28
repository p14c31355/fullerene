/// Bare-metal graphics detection using direct PCI access without UEFI protocols
pub mod bare_metal_graphics_detection {

    use crate::serial::_print;

    /// Main entry point for bare-metal graphics detection
    pub fn detect_bare_metal_graphics() -> Option<crate::common::FullereneFramebufferConfig> {
        _print(format_args!(
            "[BM-GFX] Starting bare-metal graphics detection...\n"
        ));

        // Enumerate graphics devices via direct PCI access
        let graphics_devices = crate::bare_metal_pci::enumerate_graphics_devices();

        _print(format_args!(
            "[BM-GFX] Found {} graphics devices via direct PCI enumeration\n",
            graphics_devices.len()
        ));

        // Try each graphics device for linear framebuffer detection
        for device in graphics_devices.iter() {
            _print(format_args!(
                "[BM-GFX] Probing device {:04x}:{:04x} at {:02x}:{:02x}:{:02x}\n",
                device.vendor_id, device.device_id, device.bus, device.device, device.function
            ));

            // Check for supported device types
        match (device.vendor_id, device.device_id) {
            (0x1af4, id) if id >= 0x1050 => {
                // virtio-gpu device
                _print(format_args!(
                    "[BM-GFX] Detected virtio-gpu, attempting bare-metal framebuffer detection\n"
                ));
                if let Some(config) = detect_bare_metal_virtio_gpu_framebuffer(device) {
                    _print(format_args!(
                        "[BM-GFX] Bare-metal virtio-gpu framebuffer detection successful!\n"
                    ));
                    return Some(config);
                }
            }
            (0x1b36, 0x0100) => {
                // QEMU QXL device
                _print(format_args!(
                    "[BM-GFX] Detected QXL device, attempting bare-metal framebuffer detection\n"
                ));
                if let Some(config) = detect_bare_metal_qxl_framebuffer(device) {
                    _print(format_args!(
                        "[BM-GFX] Bare-metal QXL framebuffer detection successful!\n"
                    ));
                    return Some(config);
                }
            }
            (0x1013, _) => {
                // Cirrus Logic VGA device
                _print(format_args!(
                    "[BM-GFX] Detected Cirrus Logic VGA device, attempting bare-metal framebuffer detection\n"
                ));
                if let Some(config) = detect_bare_metal_cirrus_framebuffer(device) {
                    _print(format_args!(
                        "[BM-GFX] Bare-metal Cirrus VGA framebuffer detection successful!\n"
                    ));
                    return Some(config);
                }
            }
            (0x15ad, 0x0405) => {
                // VMware SVGA II
                _print(format_args!(
                    "[BM-GFX] Detected VMware SVGA, attempting bare-metal framebuffer detection\n"
                ));
                if let Some(config) = detect_bare_metal_vmware_svga_framebuffer(device) {
                    _print(format_args!(
                        "[BM-GFX] Bare-metal VMware SVGA framebuffer detection successful!\n"
                    ));
                    return Some(config);
                }
            }
            _ => {
                _print(format_args!(
                    "[BM-GFX] Unknown graphics device type, skipping\n"
                ));
            }
        }
        }

        _print(format_args!(
            "[BM-GFX] No supported graphics devices found via bare-metal enumeration\n"
        ));
        None
    }

    /// Detect bare-metal virtio-gpu framebuffer using direct PCI BAR access
    fn detect_bare_metal_virtio_gpu_framebuffer(
        device: &crate::graphics_alternatives::PciDevice,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        // Read BAR0 from PCI configuration space directly
        let fb_base_addr =
            crate::bare_metal_pci::read_pci_bar(device.bus, device.device, device.function, 0);

        _print(format_args!(
            "[BM-GFX] virtio-gpu BAR0: {:#x}\n",
            fb_base_addr
        ));

        if fb_base_addr == 0 {
            _print(format_args!("[BM-GFX] virtio-gpu BAR0 is zero, invalid\n"));
            return None;
        }

        // Try standard VGA-like modes for virtio-gpu
        // These are commonly used defaults in QEMU
        let standard_modes = [
            (1024, 768, 32, fb_base_addr),
            (1280, 720, 32, fb_base_addr),
            (800, 600, 32, fb_base_addr),
            (640, 480, 32, fb_base_addr),
        ];

        if let Some(config) = crate::detect_standard_modes("virtio-gpu", &standard_modes) {
            return Some(config);
        }

        _print(format_args!(
            "[BM-GFX] Could not determine valid virtio-gpu framebuffer configuration\n"
        ));
        None
    }

    /// Detect QXL framebuffer via direct PCI access
    fn detect_bare_metal_qxl_framebuffer(
        device: &crate::graphics_alternatives::PciDevice,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        _print(format_args!("[BM-GFX] QXL bare-metal detection starting\n"));

        // Get BAR1 (usually the primary surface/framebuffer for QXL)
        let fb_base_addr =
            crate::bare_metal_pci::read_pci_bar(device.bus, device.device, device.function, 1);

        _print(format_args!("[BM-GFX] QXL BAR1: {:#x}\n", fb_base_addr));

        if fb_base_addr == 0 {
            _print(format_args!("[BM-GFX] QXL BAR1 is zero, invalid\n"));
            return None;
        }

        // QXL typically uses 32-bit mode in QEMU, common resolutions for QXL:
        // 1024x768 or 800x600, with 32 bits per pixel
        let standard_modes = [
            (1024, 768, 32, fb_base_addr),
            (1280, 720, 32, fb_base_addr),
            (800, 600, 32, fb_base_addr),
            (640, 480, 32, fb_base_addr),
        ];

        if let Some(config) = crate::detect_standard_modes("QXL", &standard_modes) {
            return Some(config);
        }

        _print(format_args!(
            "[BM-GFX] Could not determine valid QXL framebuffer configuration\n"
        ));
        None
    }

    /// Detect Cirrus VGA framebuffer via direct PCI access
    fn detect_bare_metal_cirrus_framebuffer(
        device: &crate::graphics_alternatives::PciDevice,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        _print(format_args!("[BM-GFX] Cirrus VGA bare-metal detection starting\n"));

        // Try BAR0 first
        let fb_base_addr =
            crate::bare_metal_pci::read_pci_bar(device.bus, device.device, device.function, 0);

        _print(format_args!("[BM-GFX] Cirrus VGA BAR0: {:#x}\n", fb_base_addr));

        // If BAR0 is 0, try standard VGA address for Cirrus
        let fb_addr = if fb_base_addr == 0 {
            _print(format_args!("[BM-GFX] Using standard VGA address for Cirrus\n"));
            0xA0000 // Standard VGA graphics buffer for Cirrus VGA
        } else {
            fb_base_addr
        };

        // Cirrus VGA typically supports various resolutions in QEMU
        // Common resolutions to try
        let standard_modes = [
            (1024, 768, 32, fb_addr),
            (800, 600, 32, fb_addr),
            (1024, 768, 24, fb_addr),
            (800, 600, 24, fb_addr),
        ];

        if let Some(config) = crate::detect_standard_modes("Cirrus VGA", &standard_modes) {
            return Some(config);
        }

        // Fall back to standard VGA mode 13h
        _print(format_args!(
            "[BM-GFX] Trying standard VGA mode 13h (320x200x8) for Cirrus\n"
        ));
        let vga_config = crate::common::memory::create_framebuffer_config(
            0xA0000,
            320,
            200,
            crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
            8,
            320,
        );

        // Check if VGA buffer is accessible
        let test_ptr = 0xA0000 as *mut u8;
        unsafe {
            // Simple test write to see if accessible
            let original = test_ptr.read_volatile();
            test_ptr.write_volatile(0xAB);
            let readback = test_ptr.read_volatile();
            test_ptr.write_volatile(original); // Restore

            if readback == 0xAB {
                _print(format_args!(
                    "[BM-GFX] VGA buffer accessible, using mode 13h\n"
                ));
                Some(vga_config)
            } else {
                _print(format_args!(
                    "[BM-GFX] VGA buffer not accessible for Cirrus\n"
                ));
                None
            }
        }
    }

    /// Detect VMware SVGA framebuffer via direct PCI access (placeholder)
    fn detect_bare_metal_vmware_svga_framebuffer(
        _device: &crate::graphics_alternatives::PciDevice,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        _print(format_args!(
            "[BM-GFX] VMware SVGA bare-metal detection not yet implemented\n"
        ));
        // VMware SVGA II uses FIFO commands and register-based communication
        // Would need to implement FIFO ring buffer management
        None
    }
}

pub use bare_metal_graphics_detection::*;
