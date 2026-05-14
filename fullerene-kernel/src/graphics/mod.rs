use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};
use petroleum::graphics::{Console, Renderer, UefiFramebufferWriter};
use petroleum::graphics::text::VgaBuffer;
use petroleum::write_serial_bytes;
use alloc::boxed::Box;
use spin::Mutex;

/// Global primary console — stored as a concrete type (no Box, no allocator).
pub static PRIMARY_CONSOLE: Mutex<Option<UefiFramebufferWriter>> = Mutex::new(None);
/// Global primary renderer — stored as a concrete type (no Box, no allocator).
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
    if GRAPHICS_INITIALIZED.swap(true, Ordering::SeqCst) {
        petroleum::debug_log!("Graphics: Already initialized, skipping\n");
        return;
    }

    // 1 & 2: Try GOP / Framebuffer
    let config = petroleum::FULLERENE_FRAMEBUFFER_CONFIG.get().and_then(|mutex| {
        let lock = mutex.lock();
        *lock
    }).or_else(|| petroleum::kernel_fallback_framebuffer_detection());

    if let Some(fb_config) = config {
        petroleum::debug_log!("Initializing graphics: GOP Framebuffer mode");
        
        let info = petroleum::graphics::color::FramebufferInfo {
            address: fb_config.address + (petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64),
            width: fb_config.width,
            height: fb_config.height,
            stride: fb_config.stride,
            pixel_format: Some(fb_config.pixel_format),
            colors: petroleum::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
        };

        let writer = petroleum::UefiFramebufferWriter::Uefi32(
            petroleum::graphics::framebuffer::FramebufferWriter::<u32>::new(info)
        );

        let mut writer_mut = writer.clone();
        petroleum::graphics::console::Console::clear(&mut writer_mut);

        *PRIMARY_CONSOLE.lock() = Some(writer.clone());
        *PRIMARY_RENDERER.lock() = Some(writer);
        
        petroleum::debug_log!("Graphics initialized with GOP Framebuffer");
        return;
    }

    // 3: Fallback to Legacy VGA Text Mode
    petroleum::debug_log!("Initializing graphics: Falling back to Legacy VGA Text Mode");
    // We use a dummy address for init_vga as it's handled internally or via constants
    crate::vga::init_vga_legacy();

    // Initialize VGA text console as fallback display
    let mut vga = VgaBuffer::with_address(
        petroleum::page_table::constants::VGA_MEMORY_START as usize
    );
    vga.enable();
    Console::clear(&mut vga);
    *VGA_CONSOLE.lock() = Some(vga);
}

pub fn set_primary_console(console: UefiFramebufferWriter) {
    *PRIMARY_CONSOLE.lock() = Some(console);
}

pub fn set_primary_renderer(renderer: UefiFramebufferWriter) {
    *PRIMARY_RENDERER.lock() = Some(renderer);
}

/// Helper to write to the primary console (with VGA fallback).
pub fn print_to_console(s: &str) {
    let mut console = PRIMARY_CONSOLE.lock();
    if let Some(ref mut console) = *console {
        let _ = console.write_str(s);
        return;
    }
    drop(console);
    // Fallback to VGA text console
    let mut vga = VGA_CONSOLE.lock();
    if let Some(ref mut vga) = *vga {
        let _ = core::fmt::write(vga, format_args!("{}", s));
    }
}

/// Helper to write formatted text to the primary console (with VGA fallback).
pub fn print_fmt(args: core::fmt::Arguments) {
    let mut console = PRIMARY_CONSOLE.lock();
    if let Some(ref mut console) = *console {
        let _ = core::fmt::write(console, args);
        return;
    }
    drop(console);
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