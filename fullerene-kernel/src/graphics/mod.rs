use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};
use petroleum::graphics::text::VgaBuffer;
use petroleum::graphics::{Console, Renderer, UefiFramebufferWriter};
use spin::Mutex;

// Import the PCI CFG reading function
use petroleum::virtio::pci::read_virtio_reg_via_pci_cfg;

/// Global primary framebuffer renderer (also used as text console).
pub static PRIMARY_RENDERER: Mutex<Option<UefiFramebufferWriter>> = Mutex::new(None);

/// Global VirtIO GPU device.
pub static VIRTIO_GPU: Mutex<Option<petroleum::virtio::gpu::VirtioGpu>> = Mutex::new(None);

/// Guard flag to prevent double initialization of the graphics subsystem.
static GRAPHICS_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Fallback VGA text console (used when UEFI framebuffer is not available).
static VGA_CONSOLE: Mutex<Option<VgaBuffer>> = Mutex::new(None);

/// Initializes the system graphics and primary console.
///
/// This function is idempotent: calling it more than once has no effect.
///
/// Priority:
/// 1. GOP Framebuffer (from bootloader config)
/// 2. Fallback GOP detection (QEMU/etc)
/// 3. Legacy VGA Text Mode (fallback)
pub fn init_graphics() {
    // Force reset GRAPHICS_INITIALIZED to handle un-zeroed .bss after world switch.
    GRAPHICS_INITIALIZED.store(false, Ordering::SeqCst);
    if GRAPHICS_INITIALIZED.swap(true, Ordering::SeqCst) {
        petroleum::debug_log!("Graphics: Already initialized, skipping\n");
        return;
    }

    if let Some(gpu_device) = {
        let mut scanner = petroleum::hardware::pci::PciScanner::new();
        let _ = scanner.scan_all_buses();
        scanner
            .get_devices()
            .iter()
            .find(|d| d.vendor_id == 0x1af4 && d.device_id == 0x1050)
            .cloned()
    } {
        petroleum::debug_log!("VirtIO-GPU: Initializing display\n");
        let common_cap = petroleum::virtio::pci::find_virtio_capability(
            &gpu_device,
            petroleum::virtio::pci::VIRTIO_PCI_CAP_COMMON_CFG,
        )
        .expect("Failed to find common capability");
        let notify_cap = petroleum::virtio::pci::find_virtio_capability(
            &gpu_device,
            petroleum::virtio::pci::VIRTIO_PCI_CAP_NOTIFY_CFG,
        )
        .expect("Failed to find notify capability");

        let common_bar = common_cap.bar;
        let common_offset = common_cap.offset;
        let notify_bar = notify_cap.bar;
        let notify_offset = notify_cap.offset;

        petroleum::serial::serial_log(format_args!(
            "[graphics] common_cap: bar={}, offset={:#x}\n",
            common_bar, common_offset
        ));

        let bar_info = gpu_device.get_bar_info(common_bar).expect("Failed to get BAR info");
        petroleum::serial::serial_log(format_args!(
            "[graphics] BAR info: address={:#x}, size={:#x}, is_64bit={}\n",
            bar_info.address, bar_info.size, bar_info.is_64bit
        ));

        // Use the GPU device found earlier in the function
        let gpu_device = gpu_device;

        let caps = petroleum::virtio::pci::get_virtio_caps(&gpu_device);
        
        let mut common_cap = None;
        let mut notify_cap = None;
        
        for cap in &caps {
            if cap.cfg_type == petroleum::virtio::pci::VIRTIO_PCI_CAP_COMMON_CFG {
                common_cap = Some(cap);
            } else if cap.cfg_type == petroleum::virtio::pci::VIRTIO_PCI_CAP_NOTIFY_CFG {
                notify_cap = Some(cap);
            }
        }
        
        // Dump all capabilities for debugging
        petroleum::virtio::pci::dump_capabilities(&gpu_device);

        // ...
        let common_cap = common_cap.expect("Common config not found");
        let notify_cap = notify_cap.expect("Notify config not found");

        petroleum::serial::serial_log(format_args!("[graphics] Dumping BAR registers for device at {}:{}\n", gpu_device.bus, gpu_device.device));
        for i in 0..6 {
            let offset = 0x10 + (i * 4);
            let val = petroleum::hardware::pci::PciConfigSpace::read_config_dword(gpu_device.bus, gpu_device.device, gpu_device.function, offset as u8);
            petroleum::serial::serial_log(format_args!("[graphics] BAR{} (offset {:#x}) = {:#x}\n", i, offset, val));
        }

        let bar_info = gpu_device.get_bar_info(common_cap.bar).expect("Failed to get Common BAR info");
        let notify_bar_info = gpu_device.get_bar_info(notify_cap.bar).expect("Failed to get Notify BAR info");

        let common_bar = common_cap.bar;
        let common_offset = common_cap.offset;
        let notify_bar = notify_cap.bar;
        let notify_offset = notify_cap.offset;

        petroleum::serial::serial_log(format_args!(
            "[graphics] BARs: common_bar={}, addr={:#x}, offset={:#x}; notify_bar={}, addr={:#x}, offset={:#x}\n",
            common_bar, bar_info.address, common_offset,
            notify_bar, notify_bar_info.address, notify_offset
        ));

        let common_virt = 0xffff800060000000;
        let notify_virt = 0xffff800070000000;

        // Enable memory access AND bus mastering
        let mut config = petroleum::hardware::pci::PciConfigSpace::read_from_device(gpu_device.bus, gpu_device.device, gpu_device.function).expect("Failed to read config");
        config.enable_memory_access(gpu_device.bus, gpu_device.device, gpu_device.function);
        // Bit 2 is Bus Master (offset 0x04)
        config.command |= 0x0004;
        let val = (config.status as u32) << 16 | (config.command as u32);
        
        petroleum::hardware::pci::PciConfigSpace::write_config_dword(
            &mut config,
            gpu_device.bus, 
            gpu_device.device, 
            gpu_device.function, 
            0x04, 
            val
        );

        // Map the full BARs.
        let mut mm = crate::memory_management::get_memory_manager().lock();
        let mm = mm.as_mut().expect("MemoryManager not initialized");
        
        let flags = petroleum::page_table::types::Flags::DEVICE_MMIO;
        mm.map_mmio_region(bar_info.address as usize, common_virt, bar_info.size as usize).unwrap();
        mm.map_mmio_region(notify_bar_info.address as usize, notify_virt, notify_bar_info.size as usize).unwrap();

        let common_virt_ptr = common_virt as *mut u32;
        let notify_virt_ptr = notify_virt as *mut u32;

        let gpu_result = petroleum::virtio::gpu::init_virtio_gpu(common_virt_ptr, notify_virt_ptr, gpu_device.clone(), common_bar);
        if gpu_result.is_none() {
            petroleum::serial::serial_log(format_args!("[graphics] init_virtio_gpu returned None!\n"));
        }

        if let Some(mut gpu) = gpu_result
        {
            petroleum::serial::serial_log(format_args!("[VirtIO-GPU] Setting up control queue...\n"));

            let (desc_virt, desc_phys) = petroleum::virtio::gpu::VirtioGpu::alloc_queue_mem(1024 * core::mem::size_of::<petroleum::virtio::gpu::VringDesc>());
            let (avail_virt, avail_phys) = petroleum::virtio::gpu::VirtioGpu::alloc_queue_mem(core::mem::size_of::<petroleum::virtio::gpu::VringAvail>());
            let (used_virt, used_phys)   = petroleum::virtio::gpu::VirtioGpu::alloc_queue_mem(core::mem::size_of::<petroleum::virtio::gpu::VringUsed>());

            petroleum::serial::serial_log(format_args!(
                "[graphics] Allocated queues: desc_p={:#x}, avail_p={:#x}, used_p={:#x}\n",
                desc_phys, avail_phys, used_phys
            ));

            let desc = desc_virt as *mut petroleum::virtio::gpu::VringDesc;
            let avail = avail_virt as *mut petroleum::virtio::gpu::VringAvail;
            let used = used_virt as *mut petroleum::virtio::gpu::VringUsed;

            gpu.setup_queue(0, desc, desc_phys, avail, avail_phys, used, used_phys);

            let mut pci_cfg_cap = None;

            for cap in &caps {
                if cap.cfg_type == petroleum::virtio::pci::VIRTIO_PCI_CAP_PCI_CFG {
                    pci_cfg_cap = Some(cap);
                }
            }

            if let Some(cfg_cap) = pci_cfg_cap {
                // Copy fields from packed struct to avoid alignment issues
                let bar = cfg_cap.bar;
                let offset = cfg_cap.offset;
                petroleum::serial::serial_log(format_args!("[graphics] Found PCI_CFG capability, bar={}, offset={:#x}\n", bar, offset));
                // Read Device ID at offset 0 via indirect access
                if let Some(bar_info) = gpu_device.get_bar_info(bar) {
                    let base = bar_info.address as usize;
                    let indirect_base = base + offset as usize;
                    // Write target offset (0x0) to address register
                    unsafe { core::ptr::write_volatile(indirect_base as *mut u32, 0x0); }
                    // Read value from data register
                    let dev_id = unsafe { core::ptr::read_volatile(indirect_base as *mut u32) };
                    petroleum::serial::serial_log(format_args!("[graphics] Device ID via indirect access: {:#x}\n", dev_id));
                } else {
                    petroleum::serial::serial_log(format_args!("[graphics] PCI_CFG capability found but BAR {} not accessible\n", bar));
                }

                // Test: Read some common config registers via Type 5 (PCI_CFG) capability
                // This is an alternative to direct BAR mapping, which may not work in some environments
                if let Some(status) = petroleum::virtio::pci::read_virtio_reg_via_pci_cfg(&gpu_device, bar, 0x14, 1) {
                    petroleum::serial::serial_log(format_args!("[graphics] Device status via Type 5: {:#x}\n", status));
                }
                if let Some(features) = petroleum::virtio::pci::read_virtio_reg_via_pci_cfg(&gpu_device, bar, 0x00, 4) {
                    petroleum::serial::serial_log(format_args!("[graphics] Device features via Type 5: {:#x}\n", features));
                }
                if let Some(queue_select) = petroleum::virtio::pci::read_virtio_reg_via_pci_cfg(&gpu_device, bar, 0x16, 2) {
                    petroleum::serial::serial_log(format_args!("[graphics] Queue select via Type 5: {:#x}\n", queue_select));
                }
            }

            // Check if we have a framebuffer configuration from UEFI
            let fb_config = petroleum::FULLERENE_FRAMEBUFFER_CONFIG.get().and_then(|mutex| mutex.lock().clone());
            let fb_info = if let Some(c) = fb_config {
                petroleum::graphics::color::FramebufferInfo {
                    address: c.address,
                    width: c.width,
                    height: c.height,
                    stride: c.stride,
                    pixel_format: Some(c.pixel_format),
                    colors: petroleum::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
                }
            } else {
                petroleum::serial::serial_log(format_args!("[graphics] No UEFI config, using fallback for VirtIO-GPU\n"));
                petroleum::graphics::color::FramebufferInfo {
                    address: 0x40000000,
                    width: 1024,
                    height: 768,
                    stride: 1024,
                    pixel_format: Some(petroleum::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor),
                    colors: petroleum::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
                }
            };

            petroleum::serial::serial_log(format_args!("[graphics] Initializing VirtIO-GPU display: {}x{}\n", fb_info.width, fb_info.height));
            gpu.init_display(fb_info.width, fb_info.height, fb_info.address, fb_info.stride * fb_info.height * 4);

            let writer = petroleum::graphics::framebuffer::FramebufferWriter::<u32>::new(fb_info);
            let renderer = petroleum::graphics::framebuffer::UefiFramebufferWriter::Uefi32(writer);

            set_primary_renderer(renderer);
            *VIRTIO_GPU.lock() = Some(gpu);
            petroleum::serial::serial_log(format_args!("[graphics] VirtIO-GPU assigned as PRIMARY_RENDERER using configuration\n"));
        }
    }

    // Try to create primary console from petroleum
    if let Some(primary_renderer) = petroleum::boot::create_primary_console() {
        *PRIMARY_RENDERER.lock() = Some(primary_renderer);
        petroleum::debug_log!("Graphics initialized with GOP Framebuffer");
        return;
    }

    // Fallback to VGA
    petroleum::debug_log!("Graphics: GOP failed, falling back to VGA text mode.\n");
    let mut vga = petroleum::boot::initialize_vga_fallback();
    vga.enable();
    petroleum::graphics::Console::clear(&mut vga);
    
    // Create the writer for VGA
    let vga_writer = petroleum::graphics::framebuffer::UefiFramebufferWriter::Vga8(
        petroleum::graphics::framebuffer::FramebufferWriter::<u8>::new(
            petroleum::graphics::color::FramebufferInfo {
                address: petroleum::page_table::constants::VGA_MEMORY_START as u64,
                width: 80,
                height: 25,
                stride: 80,
                pixel_format: None,
                colors: petroleum::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
            }
        )
    );
    
    *VGA_CONSOLE.lock() = Some(vga);
    *PRIMARY_RENDERER.lock() = Some(vga_writer);
    petroleum::debug_log!("Graphics: VGA console set as PRIMARY_RENDERER.\n");
}

/// Set the primary framebuffer renderer (also used as text console).
pub fn set_primary_renderer(renderer: UefiFramebufferWriter) {
    *PRIMARY_RENDERER.lock() = Some(renderer);
}

/// Helper to flush the GPU if present.
pub fn flush_gpu() {
    let mut gpu = VIRTIO_GPU.lock();
    if let Some(ref mut gpu) = *gpu {
        if let Some(ref r) = *PRIMARY_RENDERER.lock() {
            let info = r.get_info();
            gpu.flush(info.width, info.height);
        }
    }
}

/// Helper to write to the primary renderer (with VGA fallback).
pub fn print_to_console(s: &str) {
    {
        let mut renderer = PRIMARY_RENDERER.lock();
        if let Some(ref mut r) = *renderer {
            let _ = r.write_str(s);
        } else {
            // Fallback to VGA text console
            let mut vga = VGA_CONSOLE.lock();
            if let Some(ref mut vga) = *vga {
                let _ = core::fmt::write(vga, format_args!("{}", s));
            }
        }
    }
    flush_gpu();
}

/// Helper to write formatted text to the primary renderer (with VGA fallback).
pub fn print_fmt(args: core::fmt::Arguments) {
    {
        let mut renderer = PRIMARY_RENDERER.lock();
        if let Some(ref mut r) = *renderer {
            let _ = core::fmt::write(r, args);
        } else {
            // Fallback to VGA text console
            let mut vga = VGA_CONSOLE.lock();
            if let Some(ref mut vga) = *vga {
                let _ = core::fmt::write(vga, args);
            }
        }
    }
    flush_gpu();
}

/// Internal print helper used by boot and other early stages.
pub fn _print(args: core::fmt::Arguments) {
    print_fmt(args);
}

// Re-export desktop drawing
pub use petroleum::graphics::draw_os_desktop;

// Re-export color conversion utility
pub use petroleum::graphics::color::u32_to_rgb888;