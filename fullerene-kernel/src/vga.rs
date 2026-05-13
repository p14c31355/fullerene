use core::fmt::Write;
use petroleum::common::uefi::FullereneFramebufferConfig;
use petroleum::graphics::color::{FramebufferInfo, ColorScheme};
use spin::Mutex;

/// Global framebuffer console writer — initialized once during boot.
static FRAMEBUFFER_CONSOLE: Mutex<Option<petroleum::UefiFramebufferWriter>> = Mutex::new(None);

/// Initialize the framebuffer console using the GOP framebuffer config.
pub fn init_vga(_physical_memory_offset: x86_64::VirtAddr, _vga_virt_addr: usize) {
    petroleum::debug_log!("Initializing framebuffer console (GOP mode)");

    let config = petroleum::FULLERENE_FRAMEBUFFER_CONFIG.get().and_then(|mutex| {
        let lock = mutex.lock();
        *lock
    });

    if let Some(fb_config) = config {
        petroleum::serial::serial_log(format_args!(
            "Framebuffer: phys={:#x}, {}x{}, bpp={}, stride={}\n",
            fb_config.address, fb_config.width, fb_config.height, fb_config.bpp, fb_config.stride
        ));

        // Manually construct FramebufferInfo to avoid cfg(target_os = "uefi") issues with .new()
        let info = FramebufferInfo {
            address: fb_config.address + (petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64),
            width: fb_config.width,
            height: fb_config.height,
            stride: fb_config.stride,
            pixel_format: Some(fb_config.pixel_format),
            colors: ColorScheme::UEFI_GREEN_ON_BLACK,
        };

        let writer = petroleum::UefiFramebufferWriter::Uefi32(
            petroleum::graphics::framebuffer::FramebufferWriter::<u32>::new(info)
        );

        // UefiFramebufferWriter implements Console, which has clear()
        // We need to cast or use the trait method.
        // Since UefiFramebufferWriter is an enum, we can't call clear_screen directly.
        // We use the Console trait's clear method.
        let mut writer = writer;
        petroleum::graphics::console::Console::clear(&mut writer);

        *FRAMEBUFFER_CONSOLE.lock() = Some(writer.clone());

        // Also set as the PRIMARY_CONSOLE for the graphics module
        *crate::graphics::PRIMARY_CONSOLE.lock() = Some(writer);

        petroleum::debug_log!("Framebuffer console initialized successfully");
    } else {
        petroleum::debug_log!("No framebuffer config available, trying fallback detection");

        if let Some(fb_config) = petroleum::kernel_fallback_framebuffer_detection() {
            let higher_half = petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64;
            let info = FramebufferInfo {
                address: fb_config.address + higher_half,
                width: fb_config.width,
                height: fb_config.height,
                stride: fb_config.stride,
                pixel_format: Some(fb_config.pixel_format),
                colors: ColorScheme::UEFI_GREEN_ON_BLACK,
            };

            let mut writer = petroleum::UefiFramebufferWriter::Uefi32(
                petroleum::graphics::framebuffer::FramebufferWriter::<u32>::new(info)
            );
            petroleum::graphics::console::Console::clear(&mut writer);

            *FRAMEBUFFER_CONSOLE.lock() = Some(writer.clone());
            *crate::graphics::PRIMARY_CONSOLE.lock() = Some(writer);

            petroleum::debug_log!("Fallback framebuffer console initialized");
        } else {
            petroleum::debug_log!("No framebuffer available at all");
        }
    }
}

/// Write a string to the framebuffer console.
pub fn write_to_framebuffer(s: &str) {
    let mut console = FRAMEBUFFER_CONSOLE.lock();
    if let Some(ref mut writer) = *console {
        let _ = writer.write_str(s);
    }
}

/// Clear the framebuffer screen.
pub fn clear_framebuffer() {
    let mut console = FRAMEBUFFER_CONSOLE.lock();
    if let Some(ref mut writer) = *console {
        petroleum::graphics::console::Console::clear(writer);
    }
}

/// Fallback initialization for legacy VGA text mode.
pub fn init_vga_legacy() {
    petroleum::debug_log!("Initializing legacy VGA text mode fallback");
    
    // Legacy VGA text buffer is at 0xb8000
    let vga_buffer = petroleum::graphics::text::VgaBuffer::with_address(0xb8000);
    
    // We can't easily wrap VgaBuffer into UefiFramebufferWriter because 
    // UefiFramebufferWriter is designed for framebuffers.
    // However, for a true fallback, we just want some output.
    // In a real scenario, we might implement a VgaTextWriter that implements Console.
    
    petroleum::serial::serial_log(format_args!("Legacy VGA text mode initialized (limited functionality)\n"));
}
