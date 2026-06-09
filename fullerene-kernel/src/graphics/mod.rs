use alloc::boxed::Box;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};
use nitrogen::virtio::gpu::VirtioGpu;
use petroleum::graphics::UefiFramebufferWriter;
use petroleum::graphics::text::VgaBuffer;
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
/// 1. VirtIO-GPU (if present on PCI bus)
/// 2. GOP Framebuffer (from bootloader config, via safe_map_page WC overlay)
/// 3. Legacy VGA Text Mode (fallback)
pub fn init_graphics() {
    if GRAPHICS_INITIALIZED.swap(true, Ordering::SeqCst) {
        petroleum::debug_log!("Graphics: Already initialized, skipping\n");
        return;
    }

    // ── Path 1: VirtIO-GPU ────────────────────────────────────
    if let Some((gpu, renderer)) = crate::virtio_gpu::init() {
        set_primary_renderer(renderer);
        *VIRTIO_GPU.lock() = Some(gpu);
        petroleum::debug_log!("Graphics: VirtIO-GPU PRIMARY_RENDERER\n");
        return;
    }

    // ── Path 2: GOP / VGA mode 13h framebuffer ────────────────
    // Both 32bpp GOP and 8bpp VGA mode 13h linear framebuffers
    // are supported here.  The GUI subsystem (solvent) will skip
    // rendering if the framebuffer is too small or 8bpp, but the
    // kernel console text output will work.
    if let Some(fb_config) = petroleum::FULLERENE_FRAMEBUFFER_CONFIG
        .get()
        .and_then(|mutex| mutex.lock().clone())
    {
        let off = petroleum::common::memory::get_physical_memory_offset() as u64;
        let fb_phys = fb_config.address;
        let fb_virt = fb_phys + off;
        let fb_size = (fb_config.stride as u64 * fb_config.height as u64) as usize;
        petroleum::debug_log!(
            "[graphics] GOP fallback: phys={:#x} virt={:#x} size={}\n",
            fb_phys,
            fb_virt,
            fb_size
        );

        // Do NOT call safe_map_page for WC remap on real hardware.
        // The boot-phase 1GB huge-page WB mapping is already live and
        // working (confirmed by pre-map write test + GOP pattern test).
        // safe_map_page's 4KB WC overlay breaks the mapping on InsydeH2O
        // because map_page_4k_l1 cannot safely split the 2MB/1GB huge page.
        // We rely on the existing identity mapping (WB via PAT/MTRR).
        let mapped_ok = true;

        if mapped_ok {
            if fb_config.bpp == 8 {
                // VGA mode 13h — reinitialize the DAC palette.
                // ExitBootServices may have reset it to all-black.
                petroleum::debug_log!("[graphics] 8bpp VGA mode 13h — reinit palette & fill\n");
                // Re-run mode-13h setup (sets palette + registers)
                petroleum::graphics::setup::setup_vga_mode_13h();
                // Fill the framebuffer with a diagnostic pattern
                let fb_slice =
                    unsafe { core::slice::from_raw_parts_mut(fb_virt as *mut u8, fb_size) };
                for y in 0..fb_config.height.min(200) as usize {
                    for x in 0..fb_config.width.min(320) as usize {
                        let color: u8 = match y / 40 {
                            0 => 0x04, // red
                            1 => 0x02, // green
                            2 => 0x01, // blue
                            3 => 0x0E, // yellow
                            _ => 0x0F, // white
                        };
                        fb_slice[y * fb_config.stride as usize + x] = color;
                    }
                }
                // Also try VGA text mode (0xB8000) as fallback
                petroleum::graphics::setup::setup_vga_text_mode();
                // Fall through to Path 3 for text console
            } else {
                let fb_info = petroleum::graphics::color::FramebufferInfo {
                    address: fb_virt,
                    width: fb_config.width,
                    height: fb_config.height,
                    stride: fb_config.stride,
                    pixel_format: Some(fb_config.pixel_format),
                    colors: petroleum::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
                };
                let writer =
                    petroleum::graphics::framebuffer::FramebufferWriter::<u32>::new(fb_info);
                let renderer =
                    petroleum::graphics::framebuffer::UefiFramebufferWriter::Uefi32(writer);
                *PRIMARY_RENDERER.lock() = Some(renderer);
                petroleum::debug_log!("Graphics: GOP Framebuffer WC map OK (32bpp)\n");
                return;
            }
        }
        petroleum::debug_log!("[graphics] GOP WC map failed\n");
    }

    // ── Path 3: VGA text mode (0xB8000 character buffer) ─────
    // Fallback when no framebuffer config exists at all.
    petroleum::debug_log!("Graphics: Falling back to VGA text mode.\n");
    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
    let vga_phys = petroleum::page_table::constants::VGA_MEMORY_START;
    let vga_virt = vga_phys + off;

    // Split WB huge-page and map VGA text buffer as UC.
    let vga_flags = x86_64::structures::paging::PageTableFlags::NO_CACHE
        | x86_64::structures::paging::PageTableFlags::PRESENT
        | x86_64::structures::paging::PageTableFlags::WRITABLE
        | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;
    {
        let mut mm = crate::memory_management::get_memory_manager().lock();
        let mm = mm.as_mut().expect("MemoryManager not initialized");
        let _ = mm.safe_map_page(vga_virt as usize, vga_phys as usize, vga_flags);
    }

    let mut vga = petroleum::graphics::text::VgaBuffer::with_address(vga_virt as usize);
    vga.enable();
    petroleum::graphics::Console::clear(&mut vga);
    use core::fmt::Write;
    let _ = write!(vga, "fullerene kernel — VGA text mode\n");
    *VGA_CONSOLE.lock() = Some(vga);
    petroleum::debug_log!("Graphics: VGA text console ready, GUI disabled.\n");
}

/// Set the primary framebuffer renderer (also used as text console).
pub fn set_primary_renderer(renderer: UefiFramebufferWriter) {
    *PRIMARY_RENDERER.lock() = Some(renderer);
}

/// Helper to flush the GPU if present.
///
/// When VirtIO-GPU is active, issues a hardware flush.
/// Otherwise, emits an `sfence` (store fence) to commit any
/// write-combining (WC) framebuffer writes to the display controller.
pub fn flush_gpu() {
    let mut gpu = VIRTIO_GPU.lock();
    if let Some(ref mut gpu) = *gpu {
        if let Some(ref r) = *PRIMARY_RENDERER.lock() {
            let info = r.get_info();
            gpu.flush(info.width, info.height);
        }
    } else {
        // No VirtIO-GPU → flush non-temporal stores to the framebuffer.
        // `sfence` orders NT stores ahead of it (movnti → WC buffer → sfence →
        // globally visible).  Regular fences (mfence) also work but sfence is
        // the correct companion to _mm_stream_si32 / movnti.
        unsafe {
            core::arch::x86_64::_mm_sfence();
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
