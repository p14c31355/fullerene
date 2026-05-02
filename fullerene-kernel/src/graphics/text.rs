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

// FramebufferWriter now implements Write in petroleum

#[cfg(target_os = "uefi")]
pub static WRITER_UEFI: Mutex<Option<petroleum::UefiFramebufferWriter>> = Mutex::new(None);

#[cfg(target_os = "uefi")]
pub static FRAMEBUFFER_UEFI: Mutex<Option<petroleum::UefiFramebuffer>> = Mutex::new(None);

#[cfg(not(target_os = "uefi"))]
pub static WRITER_BIOS: Mutex<Option<petroleum::FramebufferWriter<u8>>> = Mutex::new(None);

#[cfg(not(target_os = "uefi"))]
pub static FRAMEBUFFER_BIOS: Mutex<Option<petroleum::FramebufferWriter<u8>>> = Mutex::new(None);

#[cfg(target_os = "uefi")]
pub fn init(config: &FullereneFramebufferConfig) {
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [graphics::init] entered\n");

    // Initialize simple framebuffer config (Redox vesad-style)
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [graphics::init] checking bpp\n");
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

    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [graphics::init] determining writer type\n");
    // Check pixel format to determine whether to use 32-bit or 8-bit writer
    let (writer, fb_enum) = match config.pixel_format {
        petroleum::common::EfiGraphicsPixelFormat::PixelFormatMax => {
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [graphics::init] using VGA 8-bit\n");
            let vga_config = petroleum::common::VgaFramebufferConfig {
                address: config.address,
                width: config.width,
                height: config.height,
                bpp: 8,
            };
            let writer = FramebufferWriter::<u8>::new(FramebufferInfo::new_vga(&vga_config));
            (
                petroleum::UefiFramebufferWriter::Vga8(writer.clone()),
                petroleum::UefiFramebuffer::Vga8(writer),
            )
        }
        _ => {
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [graphics::init] using UEFI 32-bit\n");
            let writer = FramebufferWriter::<u32>::new(FramebufferInfo::new(config));
            (
                petroleum::UefiFramebufferWriter::Uefi32(writer.clone()),
                petroleum::UefiFramebuffer::Uefi32(writer),
            )
        }
    };

    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [graphics::init] initializing WRITER_UEFI\n");
    {
        let mut lock = WRITER_UEFI.lock();
        if lock.is_none() {
            *lock = Some(writer);
        }
    }
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [graphics::init] WRITER_UEFI done\n");
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [graphics::init] initializing FRAMEBUFFER_UEFI\n");
    {
        let mut lock = FRAMEBUFFER_UEFI.lock();
        if lock.is_none() {
            *lock = Some(fb_enum);
        }
    }
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [graphics::init] FRAMEBUFFER_UEFI done\n");
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
        let writer_enum = petroleum::UefiFramebufferWriter::Vga8(writer.clone());
        {
            let mut lock = WRITER_UEFI.lock();
            if lock.is_none() {
                *lock = Some(writer_enum);
            }
        }
        {
            let mut lock = FRAMEBUFFER_UEFI.lock();
            if lock.is_none() {
                *lock = Some(petroleum::UefiFramebuffer::Vga8(writer));
            }
        }
    }

    #[cfg(not(target_os = "uefi"))]
    {
        let writer_clone = writer.clone();
        {
            let mut lock = WRITER_BIOS.lock();
            if lock.is_none() {
                *lock = Some(writer_clone);
            }
        }
        {
            let mut lock = FRAMEBUFFER_BIOS.lock();
            if lock.is_none() {
                *lock = Some(writer);
            }
        }
    }
}

// All VGA setup is handled by petroleum's init_vga_graphics

fn print_to_graphics(args: &fmt::Arguments) {
    #[cfg(target_os = "uefi")]
    {
        let mut lock = WRITER_UEFI.lock();
        if let Some(ref mut writer_enum) = *lock {
            match writer_enum {
                petroleum::UefiFramebufferWriter::Vga8(w) => w.write_fmt(*args).ok(),
                petroleum::UefiFramebufferWriter::Uefi32(w) => w.write_fmt(*args).ok(),
            };
        }
    }
    #[cfg(not(target_os = "uefi"))]
    {
        let mut lock = WRITER_BIOS.lock();
        if let Some(ref mut writer) = *lock {
            writer.write_fmt(*args).ok();
        }
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    print_to_graphics(&args);
    #[cfg(not(target_os = "uefi"))]
    // Also output to VGA text buffer for reliable visibility
    {
        let mut lock = crate::vga::VGA_BUFFER.lock();
        if let Some(ref mut vga_writer) = *lock {
            vga_writer.write_fmt(args).ok();
            vga_writer.update_cursor();
        }
    }
}

// Fallback graphics initialization for when framebuffer config is not available
pub fn init_fallback_graphics() -> Result<(), &'static str> {
    #[cfg(target_os = "uefi")]
    {
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_fallback_graphics] detect_and_init_vga_graphics start\n");
        petroleum::graphics::detect_and_init_vga_graphics();
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_fallback_graphics] detect_and_init_vga_graphics done\n");

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
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_fallback_graphics] calling init()\n");
        init(&fullerene_config);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_fallback_graphics] init() done\n");
    }
    #[cfg(not(target_os = "uefi"))]
    {
        // For BIOS, VGA graphics is handled separately if needed
        petroleum::info_log!("Graphics: Skipping graphics init on BIOS (handled elsewhere)");
    }
    Ok(())
}

// print! and println! macros moved to petroleum::common::macros for consistency
