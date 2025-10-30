use alloc::boxed::Box; // Import Box
use core::fmt::{self, Write};
use embedded_graphics::{
    geometry::Point,
    mono_font::{MonoTextStyle, ascii::FONT_6X10},
    prelude::*,
    text::Text,
};

use petroleum::common::FullereneFramebufferConfig;
use petroleum::common::VgaFramebufferConfig;
use petroleum::graphics::init_vga_graphics;
use spin::{Mutex, Once};

// Imports from petroleum
use petroleum::FramebufferLike;
use petroleum::FramebufferWriter;
use petroleum::graphics::color::{
    FramebufferInfo, PixelType, SimpleFramebufferConfig, init_simple_framebuffer_config,
};

// Text rendering handled by FramebufferWriter::write_str in petroleum

// Convenience type aliases
type UefiFramebufferWriter = FramebufferWriter<u32>;
type VgaFramebufferWriter = FramebufferWriter<u8>;

// FramebufferWriter now implements Write in petroleum

#[cfg(target_os = "uefi")]
pub static WRITER_UEFI: Once<Mutex<Box<dyn core::fmt::Write + Send + Sync>>> = Once::new();

#[cfg(target_os = "uefi")]
pub static FRAMEBUFFER_UEFI: Once<Mutex<petroleum::UefiFramebuffer>> = Once::new();

#[cfg(not(target_os = "uefi"))]
pub static WRITER_BIOS: Once<Mutex<Box<dyn core::fmt::Write + Send + Sync>>> = Once::new();

#[cfg(not(target_os = "uefi"))]
pub static FRAMEBUFFER_BIOS: Once<Mutex<super::framebuffer::FramebufferWriter<u8>>> = Once::new();

#[cfg(target_os = "uefi")]
pub fn init(config: &FullereneFramebufferConfig) {
    petroleum::info_log!(
        "Graphics: Initializing UEFI framebuffer: {}x{}, stride: {}, pixel_format: {:?}",
        config.width,
        config.height,
        config.stride,
        config.pixel_format
    );

    // Initialize simple framebuffer config (Redox vesad-style)
    if config.bpp == 32 {
        let simple_config = SimpleFramebufferConfig {
            base_addr: config.address as usize,
            width: config.width as usize,
            height: config.height as usize,
            stride: config.stride as usize * ((config.bpp / 8) as usize),

            bytes_per_pixel: 4, // Assume 32-bit pixels for UEFI graphics
        };
        init_simple_framebuffer_config(simple_config);
    }

    // Check pixel format to determine whether to use 32-bit or 8-bit writer
    let (writer, fb_enum) = match config.pixel_format {
        petroleum::common::EfiGraphicsPixelFormat::PixelFormatMax => {
            // Special marker for VGA mode
            petroleum::info_log!("Graphics: Using VGA 8-bit mode for UEFI");
            let vga_config = petroleum::common::VgaFramebufferConfig {
                address: config.address,
                width: config.width,
                height: config.height,
                bpp: 8,
            };
            let writer = FramebufferWriter::<u8>::new(FramebufferInfo::new_vga(&vga_config));
            (
                Box::new(writer.clone()) as Box<dyn core::fmt::Write + Send + Sync>,
                petroleum::UefiFramebuffer::Vga8(writer),
            )
        }
        _ => {
            // Standard UEFI graphics mode (32-bit)
            petroleum::info_log!("Graphics: Using 32-bit UEFI graphics mode");
            let writer = FramebufferWriter::<u32>::new(FramebufferInfo::new(config));
            (
                Box::new(writer.clone()) as Box<dyn core::fmt::Write + Send + Sync>,
                petroleum::UefiFramebuffer::Uefi32(writer),
            )
        }
    };

    WRITER_UEFI.call_once(|| Mutex::new(writer));
    FRAMEBUFFER_UEFI.call_once(|| Mutex::new(fb_enum));
}

// VgaPorts is imported from petroleum

/// Initializes VGA graphics mode 13h (320x200, 256 colors).
///
/// This function configures the VGA controller registers to switch to the specified
/// graphics mode. It is a complex process involving multiple sets of registers.
/// The initialization is broken down into smaller helper functions for clarity.
pub fn init_vga(config: &VgaFramebufferConfig) {
    init_vga_graphics(); // Use petroleum function

    let writer = FramebufferWriter::<u8>::new(FramebufferInfo::new_vga(config));
    writer.clear_screen();

    #[cfg(target_os = "uefi")]
    {
        WRITER_UEFI.call_once(|| Mutex::new(Box::new(writer.clone())));
        FRAMEBUFFER_UEFI
            .call_once(|| Mutex::new(petroleum::UefiFramebuffer::Vga8(writer)));
    }

    #[cfg(not(target_os = "uefi"))]
    {
        WRITER_BIOS.call_once(|| Mutex::new(Box::new(writer.clone())));
        FRAMEBUFFER_BIOS.call_once(|| Mutex::new(writer));
    }
}

// All VGA setup is handled by petroleum's init_vga_graphics

fn print_to_graphics(args: &fmt::Arguments) {
    #[cfg(target_os = "uefi")]
    let writer_option = WRITER_UEFI.get();
    #[cfg(not(target_os = "uefi"))]
    let writer_option = WRITER_BIOS.get();

    if let Some(writer) = writer_option {
        let mut writer = writer.lock();
        writer.write_fmt(*args).ok();
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    print_to_graphics(&args);
    // Also output to VGA text buffer for reliable visibility
    if let Some(vga) = crate::vga::VGA_BUFFER.get() {
        let mut vga_writer = vga.lock();
        vga_writer.write_fmt(args).ok();
        vga_writer.update_cursor();
    }
}

// Fallback graphics initialization for when framebuffer config is not available
pub fn init_fallback_graphics() -> Result<(), &'static str> {
    #[cfg(target_os = "uefi")]
    {
        // Initialize VGA graphics mode for UEFI fallback
        petroleum::graphics::detect_and_init_vga_graphics();

        // Create a basic VGA framebuffer config
        let vga_config_base = petroleum::common::VgaFramebufferConfig {
            address: 0xA0000, // Standard VGA frame buffer
            width: 320,
            height: 200,
            bpp: 8,
        };
        let fullerene_config = petroleum::common::FullereneFramebufferConfig {
            address: vga_config_base.address,
            width: vga_config_base.width,
            height: vga_config_base.height,
            pixel_format: petroleum::common::EfiGraphicsPixelFormat::PixelFormatMax, // VGA mode
            bpp: vga_config_base.bpp,
            stride: vga_config_base.width * vga_config_base.bpp / 8,
        };
        petroleum::info_log!("Graphics: Initializing VGA fallback framebuffer");
        init(&fullerene_config);
        petroleum::info_log!("Graphics: VGA fallback framebuffer initialized");
    }
    #[cfg(not(target_os = "uefi"))]
    {
        // For BIOS, VGA graphics is handled separately if needed
        petroleum::info_log!("Graphics: Skipping graphics init on BIOS (handled elsewhere)");
    }
    Ok(())
}

// print! and println! macros moved to petroleum::common::macros for consistency
