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
pub static mut WRITER_UEFI: Option<petroleum::UefiFramebufferWriter> = None;

#[cfg(target_os = "uefi")]
pub static mut FRAMEBUFFER_UEFI: Option<petroleum::UefiFramebuffer> = None;

#[cfg(not(target_os = "uefi"))]
pub static WRITER_BIOS: Mutex<Option<petroleum::FramebufferWriter<u8>>> = Mutex::new(None);

#[cfg(not(target_os = "uefi"))]
pub static FRAMEBUFFER_BIOS: Mutex<Option<petroleum::FramebufferWriter<u8>>> = Mutex::new(None);

// Removed top-level init function to avoid potential jump/stack issues during fallback

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
        unsafe {
            WRITER_UEFI = Some(writer_enum);
            FRAMEBUFFER_UEFI = Some(petroleum::UefiFramebuffer::Vga8(writer));
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
        unsafe {
            if let Some(ref mut writer_enum) = WRITER_UEFI {
                match writer_enum {
                    petroleum::UefiFramebufferWriter::Vga8(w) => w.write_fmt(*args).ok(),
                    petroleum::UefiFramebufferWriter::Uefi32(w) => w.write_fmt(*args).ok(),
                };
            }
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
}

// Fallback graphics initialization for when framebuffer config is not available
#[inline(always)]
pub fn init_fallback_graphics() -> Result<(), &'static str> {
    #[cfg(target_os = "uefi")]
    {
        let vga_config = petroleum::common::VgaFramebufferConfig {
            address: 0xA0000,
            width: 320,
            height: 200,
            bpp: 8,
        };
        let writer = FramebufferWriter::<u8>::new(FramebufferInfo::new_vga(&vga_config));

        unsafe {
            WRITER_UEFI = Some(petroleum::UefiFramebufferWriter::Vga8(writer.clone()));
            FRAMEBUFFER_UEFI = Some(petroleum::UefiFramebuffer::Vga8(writer));
        }
    }
    #[cfg(not(target_os = "uefi"))]
    {
        // For BIOS, VGA graphics is handled separately if needed
        petroleum::info_log!("Graphics: Skipping graphics init on BIOS (handled elsewhere)");
    }
    Ok(())
}

// print! and println! macros moved to petroleum::common::macros for consistency
