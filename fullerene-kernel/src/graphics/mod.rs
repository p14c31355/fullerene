use alloc::boxed::Box;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};
use petroleum::graphics::text::VgaBuffer;
use petroleum::graphics::{FramebufferBackend, GopFramebuffer};
use spin::Mutex;

/// Global primary framebuffer backend.
pub static PRIMARY_BACKEND: Mutex<Option<Box<dyn FramebufferBackend + Send>>> = Mutex::new(None);
pub static VGA_CONSOLE: Mutex<Option<VgaBuffer>> = Mutex::new(None);

/// Guard flag to prevent double initialization of the graphics subsystem.
static GRAPHICS_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initializes the system graphics and primary console.
pub fn init_graphics() {
    if GRAPHICS_INITIALIZED.swap(true, Ordering::SeqCst) {
        return;
    }

    // 1. Fallback to GOP backend
    if let Some(gop_backend) = try_init_gop() {
        *PRIMARY_BACKEND.lock() = Some(Box::new(gop_backend));
        petroleum::serial::serial_log(format_args!("[graphics] GOP initialized.\n"));
        return;
    }

    // 2. Fallback to VGA console
    petroleum::serial::serial_log(format_args!("[graphics] VGA fallback.\n"));
    let mut vga = petroleum::early::framebuffer::initialize_vga_fallback();
    vga.enable();
    *VGA_CONSOLE.lock() = Some(vga);
}

fn try_init_gop() -> Option<GopFramebuffer> {
    let fb_config = petroleum::FULLERENE_FRAMEBUFFER_CONFIG.get().and_then(|m| m.lock().clone());
    if let Some(c) = fb_config {
        let info = petroleum::graphics::color::FramebufferInfo {
            address: c.address,
            width: c.width,
            height: c.height,
            stride: c.stride,
            pixel_format: Some(c.pixel_format),
            colors: petroleum::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
        };
        Some(GopFramebuffer::new(info))
    } else {
        None
    }
}

pub fn print_fmt(args: core::fmt::Arguments) {
    let mut vga = VGA_CONSOLE.lock();
    if let Some(ref mut vga) = *vga {
        let _ = core::fmt::write(vga, args);
    }
}

pub fn flush_gpu() {}
pub fn set_primary_renderer(_: petroleum::graphics::UefiFramebufferWriter) {}
pub use petroleum::graphics::{draw_os_desktop, u32_to_rgb888};
