use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};
use petroleum::graphics::{Console, Renderer, UefiFramebufferWriter};
use petroleum::graphics::text::VgaBuffer;
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
    // This mirrors the force-reset pattern used for ALLOCATOR, HEAP_INITIALIZED,
    // and LOCAL_APIC_ADDRESS in uefi_init.rs.
    GRAPHICS_INITIALIZED.store(false, Ordering::SeqCst);
    if GRAPHICS_INITIALIZED.swap(true, Ordering::SeqCst) {
        petroleum::debug_log!("Graphics: Already initialized, skipping\n");
        return;
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