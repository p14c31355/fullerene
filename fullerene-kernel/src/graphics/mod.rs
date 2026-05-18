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
            petroleum::virtio::gpu::VIRTIO_PCI_CAP_COMMON_CFG,
        )
        .unwrap();
        let notify_cap = petroleum::virtio::pci::find_virtio_capability(
            &gpu_device,
            petroleum::virtio::gpu::VIRTIO_PCI_CAP_NOTIFY_CFG,
        )
        .unwrap();

        let bar_phys = gpu_device.read_bar(common_cap.bar).unwrap() as usize;
        let notify_bar_phys = gpu_device.read_bar(notify_cap.bar).unwrap() as usize;

        let kernel_offset = petroleum::page_table::constants::HIGHER_HALF_OFFSET.as_u64() as usize;
        let common_virt = 0xffff800060000000;
        let notify_virt = 0xffff800060004000;

        let mut mm = crate::memory_management::get_memory_manager().lock();
        let mm = mm.as_mut().expect("MemoryManager not initialized");

        petroleum::serial::serial_log(format_args!(
            "[graphics] Mapping BARs to virt={:#x}, {:#x}\n",
            common_virt, notify_virt
        ));

        mm.map_mmio_region(bar_phys, common_virt, 0x4000).unwrap();
        mm.map_mmio_region(notify_bar_phys, notify_virt, 0x4000)
            .unwrap();

        let common_virt_ptr = (common_virt + common_cap.offset as usize) as *mut u32;
        let notify_virt_ptr = (notify_virt + notify_cap.offset as usize) as *mut u32;

        if let Some(mut gpu) =
            petroleum::virtio::gpu::VirtioGpu::init_virtio_gpu(common_virt_ptr, notify_virt_ptr)
        {
            gpu.init_display(1024, 768, 0xfc000000, 1024 * 768 * 4);
            return;
        }
    }

    // Try to create primary console from petroleum
    if let Some(primary_renderer) = petroleum::boot::create_primary_console() {
        *PRIMARY_RENDERER.lock() = Some(primary_renderer);
        petroleum::debug_log!("Graphics initialized with GOP Framebuffer");
        return;
    }

    // Fallback to VGA
    let mut vga = petroleum::boot::initialize_vga_fallback();
    vga.enable();
    petroleum::graphics::Console::clear(&mut vga);
    *VGA_CONSOLE.lock() = Some(vga);
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
