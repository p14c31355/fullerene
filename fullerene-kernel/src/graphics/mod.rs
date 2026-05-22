use alloc::boxed::Box;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};
use petroleum::graphics::text::VgaBuffer;
use petroleum::graphics::{Console, Renderer, UefiFramebufferWriter};
use nitrogen::virtio::gpu::VirtioGpu;
use spin::Mutex;

/// Global primary framebuffer renderer (also used as text console).
pub static PRIMARY_RENDERER: Mutex<Option<UefiFramebufferWriter>> = Mutex::new(None);

/// Global VirtIO GPU device.
pub static VIRTIO_GPU: Mutex<Option<Box<VirtioGpu>>> = Mutex::new(None);

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
    // Note: .bss is guaranteed zeroed by the bootloader, so GRAPHICS_INITIALIZED
    // starts as false.  If this function is called more than once the swap will
    // detect it and return early.
    if GRAPHICS_INITIALIZED.swap(true, Ordering::SeqCst) {
        petroleum::debug_log!("Graphics: Already initialized, skipping\n");
        return;
    }

    if let Some(ref gpu_device) = {
        let mut scanner = nitrogen::pci::PciScanner::new();
        let _ = scanner.scan_all_buses();
        scanner
            .get_devices()
            .iter()
            .find(|d| d.vendor_id == 0x1af4 && d.device_id == 0x1050)
            .cloned()
    } {
        petroleum::debug_log!("VirtIO-GPU: Initializing display\n");

        // Scan capabilities once
        let caps = petroleum::virtio::pci::get_virtio_caps(gpu_device);
        petroleum::virtio::pci::dump_capabilities(gpu_device);

        let mut common_cap = None;
        let mut notify_cap = None;
        let mut pci_cfg_cap = None;

        for cap in &caps {
            match cap.cfg_type {
                petroleum::virtio::pci::VIRTIO_PCI_CAP_COMMON_CFG => common_cap = Some(cap),
                petroleum::virtio::pci::VIRTIO_PCI_CAP_NOTIFY_CFG => notify_cap = Some(cap),
                petroleum::virtio::pci::VIRTIO_PCI_CAP_PCI_CFG => pci_cfg_cap = Some(cap),
                _ => {}
            }
        }

        let common_cap = common_cap.expect("Common config not found");
        let notify_cap = notify_cap.expect("Notify config not found");

        let common_bar = common_cap.bar;
        let common_offset = common_cap.offset;
        let notify_bar = notify_cap.bar;
        let notify_offset = notify_cap.offset;

        petroleum::serial::serial_log(format_args!(
            "[graphics] common_cap: bar={}, offset={:#x}\n",
            common_bar, common_offset
        ));

        let bar_info = gpu_device.get_bar_info(common_bar).expect("Failed to get Common BAR info");
        let notify_bar_info = gpu_device.get_bar_info(notify_bar).expect("Failed to get Notify BAR info");

        petroleum::serial::serial_log(format_args!("[graphics] Dumping BAR registers for device at {}:{}\n", gpu_device.bus, gpu_device.device));
        for i in 0..6 {
            let offset = 0x10 + (i * 4);
            let val = nitrogen::pci::PciConfigSpace::read_config_dword(gpu_device.bus, gpu_device.device, gpu_device.function, offset as u8);
            petroleum::serial::serial_log(format_args!("[graphics] BAR{} (offset {:#x}) = {:#x}\n", i, offset, val));
        }

        petroleum::serial::serial_log(format_args!(
            "[graphics] BARs: common_bar={}, addr={:#x}, offset={:#x}; notify_bar={}, addr={:#x}, offset={:#x}\n",
            common_bar, bar_info.address, common_offset,
            notify_bar, notify_bar_info.address, notify_offset
        ));

        let common_virt = 0xffff800060000000u64 as usize;
        let notify_virt = 0xffff800070000000u64 as usize;

        // Enable memory access AND bus mastering
        let mut config = nitrogen::pci::PciConfigSpace::read_from_device(gpu_device.bus, gpu_device.device, gpu_device.function).expect("Failed to read config");
        let cmd = config.command;
        let stat = config.status;
        petroleum::serial::serial_log(format_args!("[graphics] Pre-enable: Command={:#x}, Status={:#x}\n", cmd, stat));
        config.enable_memory_access(gpu_device.bus, gpu_device.device, gpu_device.function);
        // Bit 2 is Bus Master (offset 0x04)
        config.command |= 0x0004;
        let val = (config.status as u32) << 16 | (config.command as u32);
        
        nitrogen::pci::PciConfigSpace::write_config_dword(
            &mut config,
            gpu_device.bus, 
            gpu_device.device, 
            gpu_device.function, 
            0x04, 
            val
        );
        let config_after = nitrogen::pci::PciConfigSpace::read_from_device(gpu_device.bus, gpu_device.device, gpu_device.function).expect("Failed to read config");
        let cmd_after = config_after.command;
        let stat_after = config_after.status;
        petroleum::serial::serial_log(format_args!("[graphics] Post-enable: Command={:#x}, Status={:#x}\n", cmd_after, stat_after));

        // Map the full BARs.
        let mut mm = crate::memory_management::get_memory_manager().lock();
        let mm = mm.as_mut().expect("MemoryManager not initialized");
        
        mm.map_mmio_region(bar_info.address as usize, common_virt, bar_info.size as usize).unwrap();
        mm.map_mmio_region(notify_bar_info.address as usize, notify_virt, notify_bar_info.size as usize).unwrap();

        let common_virt_ptr = (common_virt + common_offset as usize) as *mut u32;
        let notify_virt_ptr = (notify_virt + notify_offset as usize) as *mut u32;

        petroleum::serial::serial_log(format_args!("[graphics] Dumping first 64 bytes of common_virt+offset ({:#p}):\n", common_virt_ptr));
        for i in 0..16 {
            let val = unsafe { core::ptr::read_volatile(common_virt_ptr.add(i)) };
            petroleum::serial::serial_log(format_args!("{:#010x} ", val));
            if (i + 1) % 4 == 0 { petroleum::serial::serial_log(format_args!("\n")); }
        }

        use petroleum::page_table::constants::get_frame_allocator_mut;
        let off = petroleum::common::memory::get_physical_memory_offset() as u64;

        // Allocate command buffer (1 page)
        let cmd_raw = get_frame_allocator_mut()
            .allocate_contiguous_frames(1)
            .expect("VirtIO-GPU: failed to allocate cmd buffer");
        let cmd_buf_phys = cmd_raw as u64;
        let cmd_buf = (cmd_buf_phys + off) as *mut u8;
        unsafe { core::ptr::write_bytes(cmd_buf, 0, 4096); }

        // Allocate response buffer (1 page)
        let resp_raw = get_frame_allocator_mut()
            .allocate_contiguous_frames(1)
            .expect("VirtIO-GPU: failed to allocate resp buffer");
        let resp_buf_phys = resp_raw as u64;
        let resp_buf = (resp_buf_phys + off) as *mut u8;
        unsafe { core::ptr::write_bytes(resp_buf, 0, 4096); }

        let gpu_result = nitrogen::virtio::gpu::init_virtio_gpu(
            common_virt_ptr, notify_virt_ptr, gpu_device.clone(), common_bar,
            cmd_buf, cmd_buf_phys, 4096,
            resp_buf, resp_buf_phys, 4096,
        );

        if let Some(mut gpu) = gpu_result
        {
            petroleum::serial::serial_log(format_args!("[VirtIO-GPU] Setting up control queue...\n"));

            // Queue memory: allocated by caller (nitrogen is allocator-agnostic)
            let alloc_qmem = |size: usize| -> (*mut u8, u64) {
                let pages = (size + 4095) / 4096;
                let raw = get_frame_allocator_mut()
                    .allocate_contiguous_frames(pages)
                    .expect("VirtIO-GPU: failed to allocate queue memory");
                let phys = raw as u64;
                ((phys + off) as *mut u8, phys)
            };

            let (desc_virt, desc_phys) = alloc_qmem(1024 * core::mem::size_of::<nitrogen::virtio::gpu::VringDesc>());
            let (avail_virt, avail_phys) = alloc_qmem(core::mem::size_of::<nitrogen::virtio::gpu::VringAvail>());
            let (used_virt, used_phys)   = alloc_qmem(core::mem::size_of::<nitrogen::virtio::gpu::VringUsed>());

            petroleum::serial::serial_log(format_args!(
                "[graphics] Allocated queues: desc_p={:#x}, avail_p={:#x}, used_p={:#x}\n",
                desc_phys, avail_phys, used_phys
            ));

            let desc = desc_virt as *mut nitrogen::virtio::gpu::VringDesc;
            let avail = avail_virt as *mut nitrogen::virtio::gpu::VringAvail;
            let used = used_virt as *mut nitrogen::virtio::gpu::VringUsed;

            gpu.setup_queue(0, desc, desc_phys, avail, avail_phys, used, used_phys);

            if let Some(cfg_cap) = pci_cfg_cap {
                // NOTE: Do NOT access PCI_CFG capability via BAR indirect writes
                // (writing to BAR0 = framebuffer @ 0xc0000000 crashes QEMU).
                // Use read_virtio_reg_via_pci_cfg() which uses PCI I/O ports (0xCF8/0xCFC).
                let bar = cfg_cap.bar;
                let offset = cfg_cap.offset;
                petroleum::serial::serial_log(format_args!("[graphics] Found PCI_CFG capability, bar={}, offset={:#x}\n", bar, offset));

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
            match gpu.init_display(fb_info.width, fb_info.height, fb_info.address, fb_info.stride * fb_info.height) {
                Ok(()) => {
                    let writer = petroleum::graphics::framebuffer::FramebufferWriter::<u32>::new(fb_info);
                    let renderer = petroleum::graphics::framebuffer::UefiFramebufferWriter::Uefi32(writer);

                    set_primary_renderer(renderer);
                    *VIRTIO_GPU.lock() = Some(Box::new(gpu));
                    petroleum::serial::serial_log(format_args!("[graphics] VirtIO-GPU assigned as PRIMARY_RENDERER using configuration\n"));
                    return; // Prevent GOP fallback from overwriting the VirtIO-GPU renderer
                }
                Err(e) => {
                    petroleum::serial::serial_log(format_args!(
                        "[graphics] VirtIO-GPU init_display failed: {:?}. Falling back to GOP.\n", e
                    ));
                    // gpu is dropped here, VIRTIO_GPU is NOT set.
                    // MMIO mappings remain but won't interfere with GOP.
                }
            }
        } else {
            petroleum::serial::serial_log(format_args!(
                "[graphics] VirtIO-GPU init_virtio_gpu() returned None. Falling back to GOP.\n"
            ));
        }
    } else {
        petroleum::serial::serial_log(format_args!(
            "[graphics] No VirtIO-GPU device found. Trying GOP.\n"
        ));
    }

    // Try to create primary console from petroleum (GOP fallback)
    if let Some(primary_renderer) = petroleum::early::framebuffer::create_primary_console() {
        *PRIMARY_RENDERER.lock() = Some(primary_renderer);
        petroleum::debug_log!("Graphics initialized with GOP Framebuffer");
        return;
    }

    // Fallback to VGA
    petroleum::debug_log!("Graphics: GOP failed, falling back to VGA text mode.\n");
    let mut vga = petroleum::early::framebuffer::initialize_vga_fallback();
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

