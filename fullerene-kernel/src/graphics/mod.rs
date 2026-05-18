use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};
use petroleum::graphics::text::VgaBuffer;
use petroleum::graphics::{Console, Renderer, UefiFramebufferWriter};
use spin::Mutex;

/// Global primary framebuffer renderer (also used as text console).
pub static PRIMARY_RENDERER: Mutex<Option<UefiFramebufferWriter>> = Mutex::new(None);

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

        // Check command register
        let status_cmd = petroleum::hardware::pci::PciConfigSpace::read_config_dword(gpu_device.bus, gpu_device.device, gpu_device.function, 0x04);
        let cmd = (status_cmd & 0xFFFF) as u16;
        petroleum::serial::serial_log(format_args!("[graphics] PCI command: {:#x}\n", cmd));

        if (cmd & 0x2) == 0 {
            petroleum::serial::serial_log(format_args!("[graphics] Enabling memory access\n"));
            let mut config = petroleum::hardware::pci::PciConfigSpace::read_from_device(gpu_device.bus, gpu_device.device, gpu_device.function).expect("Failed to read config");
            config.command |= 0x2;
            let val = (status_cmd & !0xFFFF) | (config.command as u32);
            petroleum::hardware::pci::PciConfigSpace::write_config_dword(
                &mut config, 
                gpu_device.bus, 
                gpu_device.device, 
                gpu_device.function, 
                0x04, 
                val
            );
        }        let bar_phys = bar_info.address as usize;
        let notify_bar_phys = gpu_device.read_bar(notify_bar).expect("Failed to read BAR for notify_cap") as usize & !0xF;

        petroleum::serial::serial_log(format_args!(
            "[graphics] BARs: common_phys={:#x}, notify_phys={:#x}\n",
            bar_phys, notify_bar_phys
        ));
        let common_virt = 0xffff800060000000;
        let notify_virt = 0xffff800060004000;

        // Map the full BARs.
        let mut mm = crate::memory_management::get_memory_manager().lock();
        let mm = mm.as_mut().expect("MemoryManager not initialized");

        petroleum::serial::serial_log(format_args!(
            "[graphics] Mapping BARs to virt={:#x}, {:#x}\n",
            common_virt, notify_virt
        ));

        // Mapping size should match bar size
        mm.map_mmio_region(bar_phys, common_virt, bar_info.size as usize).unwrap();
        mm.map_mmio_region(notify_bar_phys, notify_virt, 0x10000).unwrap();

        // The capability offset is relative to the BAR.
        let common_virt_ptr = (common_virt + common_offset as usize) as *mut u32;
        let notify_virt_ptr = (notify_virt + notify_offset as usize) as *mut u32;

        petroleum::serial::serial_log(format_args!(
            "[graphics] VirtIO-GPU mapped pointers: common={:p}, notify={:p}\n",
            common_virt_ptr, notify_virt_ptr
        ));

        if let Some(mut gpu) =
            petroleum::virtio::gpu::VirtioGpu::init_virtio_gpu(common_virt_ptr, notify_virt_ptr)
        {
            gpu.init_display(1024, 768, 0xfc000000, 1024 * 768 * 4);
            
            // Create a UefiFramebufferWriter for the GPU
            let fb_info = petroleum::graphics::color::FramebufferInfo {
                address: 0xfc000000,
                width: 1024,
                height: 768,
                stride: 1024,
                pixel_format: Some(petroleum::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor),
                colors: petroleum::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
            };
            let writer = petroleum::graphics::framebuffer::FramebufferWriter::<u32>::new(fb_info);
            let renderer = petroleum::graphics::framebuffer::UefiFramebufferWriter::Uefi32(writer);
            
            set_primary_renderer(renderer);
            petroleum::serial::serial_log(format_args!("[graphics] VirtIO-GPU assigned as PRIMARY_RENDERER\n"));
            return;
        }    }

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

/// Helper to write to the primary renderer (with VGA fallback).
pub fn print_to_console(s: &str) {
    let mut renderer = PRIMARY_RENDERER.lock();
    if let Some(ref mut r) = *renderer {
        let _ = r.write_str(s);
        return;
    }
    drop(renderer);
    // Fallback to VGA text console
    let mut vga = VGA_CONSOLE.lock();
    if let Some(ref mut vga) = *vga {
        let _ = core::fmt::write(vga, format_args!("{}", s));
    }
}

/// Helper to write formatted text to the primary renderer (with VGA fallback).
pub fn print_fmt(args: core::fmt::Arguments) {
    let mut renderer = PRIMARY_RENDERER.lock();
    if let Some(ref mut r) = *renderer {
        let _ = core::fmt::write(r, args);
        return;
    }
    drop(renderer);
    // Fallback to VGA text console
    let mut vga = VGA_CONSOLE.lock();
    if let Some(ref mut vga) = *vga {
        let _ = core::fmt::write(vga, args);
    }
}

/// Internal print helper used by boot and other early stages.
pub fn _print(args: core::fmt::Arguments) {
    print_fmt(args);
}

// Re-export desktop drawing
pub use petroleum::graphics::draw_os_desktop;

// Re-export color conversion utility
pub use petroleum::graphics::color::u32_to_rgb888;
