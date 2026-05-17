/// Bare-metal graphics detection using direct PCI access without UEFI protocols
pub mod bare_metal_graphics_detection {

    use crate::serial::_print;
    use crate::hardware::pci::PciDevice;

    /// Main entry point for bare-metal graphics detection
    pub fn detect_bare_metal_graphics() -> Option<crate::common::FullereneFramebufferConfig> {
        _print(format_args!(
            "[BM-GFX] Starting bare-metal graphics detection...
"
        ));

        // Enumerate graphics devices via direct PCI access
        let graphics_devices = crate::bare_metal_pci::enumerate_graphics_devices();

        _print(format_args!(
            "[BM-GFX] Found {} graphics devices via direct PCI enumeration
",
            graphics_devices.len()
        ));

        // Try each graphics device for linear framebuffer detection
        for device in graphics_devices {
            if device.vendor_id == 0x1af4 && device.device_id >= 0x1050 {
                if let Some(config) = detect_bare_metal_virtio_gpu_framebuffer(&device) {
                    return Some(config);
                }
            }
        }
        
        None
    }
    
    fn detect_bare_metal_virtio_gpu_framebuffer(
        device: &PciDevice,
    ) -> Option<crate::common::FullereneFramebufferConfig> {
        _print(format_args!(
            "[BM-GFX] Initializing virtio-gpu driver for device {:04x}:{:04x}...
",
            device.vendor_id, device.device_id
        ));

        let mut gpu = match crate::virtio::gpu::VirtioGpu::new(device) {
            Ok(g) => g,
            Err(e) => {
                _print(format_args!("[BM-GFX] VirtioGpu::new failed: {:?}
", e));
                return None;
            }
        };

        if let Err(e) = gpu.init() {
            _print(format_args!("[BM-GFX] VirtioGpu::init failed: {:?}
", e));
            return None;
        }

        _print(format_args!("[BM-GFX] Virtio-GPU driver initialized successfully!
"));
        
        Some(crate::common::memory::create_framebuffer_config(
            0, 1024, 768, 
            crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor, 
            32, 4096
        ))
    }
}

pub use bare_metal_graphics_detection::*;
