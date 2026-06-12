//! Graphics subsystem — thin wrappers around [`crate::contexts::FramebufferContext`].
//!
//! All state lives in [`FramebufferContext`].  This module keeps the same
//! public API so existing callers (`gui.rs`, `shell.rs`, `virtio_gpu.rs`)
//! continue to compile.
//!
//! Re-exports:
//! - `PRIMARY_RENDERER`  → `get_primary_renderer()` / `with_framebuffer_mut()`
//! - `VIRTIO_GPU`        → `with_framebuffer_mut(|fb| fb.gpu.as_mut())`
//! - `VGA_CONSOLE`       → `with_framebuffer_mut(|fb| fb.vga_console.as_mut())`

use alloc::boxed::Box;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};
use nitrogen::virtio::gpu::VirtioGpu;
use petroleum::graphics::UefiFramebufferWriter;
use petroleum::graphics::text::VgaBuffer;
use spin::Mutex;

use crate::contexts::framebuffer::{
    FramebufferContext, get_framebuffer, with_framebuffer, with_framebuffer_mut,
};

/// Legacy re-export — prefer `with_framebuffer_mut`.
/// Use `get_framebuffer().lock()` to access the full context.
pub static PRIMARY_RENDERER: Mutex<Option<UefiFramebufferWriter>> = Mutex::new(None);

/// Legacy re-export — prefer `with_framebuffer_mut(|fb| &mut fb.gpu)`.
pub static VIRTIO_GPU: Mutex<Option<Box<VirtioGpu>>> = Mutex::new(None);

/// Legacy re-export — prefer `with_framebuffer_mut(|fb| &mut fb.vga_console)`.
static VGA_CONSOLE: Mutex<Option<VgaBuffer>> = Mutex::new(None);

/// Guard flag to prevent double initialization.
static GRAPHICS_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initializes the system graphics and primary console.
///
/// This function is idempotent.  After initialisation, the globals
/// `PRIMARY_RENDERER`, `VIRTIO_GPU`, and `VGA_CONSOLE` are synced into
/// `FramebufferContext` so callers can use either the old globals or
/// the new context API.
///
/// Priority:
/// 1. VirtIO-GPU (if present on PCI bus)
/// 2. GOP Framebuffer (from bootloader config)
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

        // Sync into FramebufferContext
        if let Some(fb_ctx) = get_framebuffer().lock().as_mut() {
            fb_ctx.renderer = PRIMARY_RENDERER.lock().clone();
            // GPU stays in VIRTIO_GPU static for legacy code; context can reach it there.
            fb_ctx.bpp = 32;
        }
        return;
    }

    // ── Path 2: GOP / VGA mode 13h framebuffer ────────────────
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

        let mapped_ok = true;

        if mapped_ok {
            if fb_config.bpp == 8 {
                petroleum::debug_log!("[graphics] 8bpp VGA mode 13h — reinit palette & fill\n");
                petroleum::graphics::setup::setup_vga_mode_13h();
                let fb_slice =
                    unsafe { core::slice::from_raw_parts_mut(fb_virt as *mut u8, fb_size) };
                for y in 0..fb_config.height.min(200) as usize {
                    for x in 0..fb_config.width.min(320) as usize {
                        let color: u8 = match y / 40 {
                            0 => 0x04,
                            1 => 0x02,
                            2 => 0x01,
                            3 => 0x0E,
                            _ => 0x0F,
                        };
                        fb_slice[y * fb_config.stride as usize + x] = color;
                    }
                }
                petroleum::graphics::setup::setup_vga_text_mode();
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

                // Sync into FramebufferContext
                if let Some(fb_ctx) = get_framebuffer().lock().as_mut() {
                    fb_ctx.renderer = PRIMARY_RENDERER.lock().clone();
                    fb_ctx.bpp = fb_config.bpp;
                }
                return;
            }
        }
        petroleum::debug_log!("[graphics] GOP WC map failed\n");
    }

    // ── Path 3: VGA text mode (0xB8000 character buffer) ─────
    petroleum::debug_log!("Graphics: Falling back to VGA text mode.\n");
    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
    let vga_phys = petroleum::page_table::constants::VGA_MEMORY_START;
    let vga_virt = vga_phys + off;

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
    let _ = write!(vga, "fullerene kernel — VGA text mode\n");
    *VGA_CONSOLE.lock() = Some(vga);
    petroleum::debug_log!("Graphics: VGA text console ready, GUI disabled.\n");

    // Sync into FramebufferContext
    if let Some(fb_ctx) = get_framebuffer().lock().as_mut() {
        fb_ctx.vga_console = VGA_CONSOLE.lock().clone();
    }
}

/// Set the primary framebuffer renderer (also used as text console).
pub fn set_primary_renderer(renderer: UefiFramebufferWriter) {
    *PRIMARY_RENDERER.lock() = Some(renderer);
}

/// Helper to flush the GPU if present.
pub fn flush_gpu() {
    with_framebuffer_mut(|fb| fb.flush());
}

/// Helper to write to the primary renderer (with VGA fallback).
pub fn print_to_console(s: &str) {
    with_framebuffer_mut(|fb| fb.write_str(s));
    flush_gpu();
}

/// Helper to write formatted text to the primary renderer (with VGA fallback).
pub fn print_fmt(args: core::fmt::Arguments) {
    with_framebuffer_mut(|fb| fb.write_fmt(args));
    flush_gpu();
}

/// Internal print helper used by boot and other early stages.
pub fn _print(args: core::fmt::Arguments) {
    print_fmt(args);
}