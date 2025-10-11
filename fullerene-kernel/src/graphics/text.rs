use core::fmt::{self, Write};
use alloc::boxed::Box; // Import Box
use embedded_graphics::{
    geometry::Point,
    mono_font::{MonoTextStyle, ascii::FONT_6X10},
    pixelcolor::*,
    prelude::*,
    text::Text,
};

use petroleum::common::VgaFramebufferConfig;
use petroleum::common::{EfiGraphicsPixelFormat, FullereneFramebufferConfig};
use petroleum::serial::debug_print_str_to_com1 as debug_print_str;
use petroleum::graphics::init_vga_graphics;
use petroleum::{clear_buffer_pixels, scroll_buffer_pixels};
use spin::{Mutex, Once};

// Imports from other modules
use super::framebuffer::{FramebufferLike, FramebufferWriter, FramebufferInfo};

// Optimized text rendering using embedded-graphics
// Batcher processing for efficiency and reduced code complexity
fn write_text<W: FramebufferLike>(writer: &mut W, s: &str) -> core::fmt::Result {
    const CHAR_WIDTH: i32 = FONT_6X10.character_size.width as i32;
    const CHAR_HEIGHT: i32 = FONT_6X10.character_size.height as i32;

    let fg_color = super::u32_to_rgb888(writer.get_fg_color());

    let style = MonoTextStyle::new(&FONT_6X10, fg_color);
    let mut lines = s.split_inclusive('\n');
    let mut current_pos = Point::new(
        writer.get_position().0 as i32,
        writer.get_position().1 as i32,
    );

    for line_with_newline in lines {
        // Handle the line (including newline if present)
        let has_newline = line_with_newline.ends_with('\n');
        let line_content = if has_newline {
            &line_with_newline[..line_with_newline.len() - 1]
        } else {
            line_with_newline
        };

        // Render the entire line at once for efficiency
        if !line_content.is_empty() {
            let text = Text::new(line_content, current_pos, style);
            text.draw(writer).ok();

            // Advance position by the rendered text width
            current_pos.x += CHAR_WIDTH * line_content.chars().count() as i32;
        }

        if has_newline {
            current_pos.x = 0;
            current_pos.y += CHAR_HEIGHT; // Font height

            // Handle scrolling if needed
            if current_pos.y + CHAR_HEIGHT > writer.get_height() as i32 {
                writer.scroll_up();
                current_pos.y -= CHAR_HEIGHT;
            }
        } else {
            // Handle line wrapping for lines without explicit newlines
            if current_pos.x >= writer.get_width() as i32 {
                current_pos.x = 0;
                current_pos.y += CHAR_HEIGHT;
                if current_pos.y + CHAR_HEIGHT > writer.get_height() as i32 {
                    writer.scroll_up();
                    current_pos.y -= CHAR_HEIGHT;
                }
            }
        }
    }

    writer.set_position(current_pos.x as u32, current_pos.y as u32);
    Ok(())
}

fn unsupported_pixel_format_log() {
    petroleum::serial::serial_log(format_args!(
        "Warning: Pixel format not supported, using RGB fallback\n"
    ));
}

// Convenience type aliases
type UefiFramebufferWriter = FramebufferWriter<u32>;
type VgaFramebufferWriter = FramebufferWriter<u8>;

impl<T> core::fmt::Write for FramebufferWriter<T>
where
    T: super::framebuffer::PixelType,
{
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        write_text(self, s)
    }
}

#[cfg(target_os = "uefi")]
pub static WRITER_UEFI: Once<Mutex<Box<dyn core::fmt::Write + Send + Sync>>> = Once::new();

#[cfg(target_os = "uefi")]
pub static FRAMEBUFFER_UEFI: Once<Mutex<super::framebuffer::UefiFramebuffer>> = Once::new();

#[cfg(not(target_os = "uefi"))]
pub static WRITER_BIOS: Once<Mutex<Box<dyn core::fmt::Write + Send + Sync>>> = Once::new();

#[cfg(not(target_os = "uefi"))]
pub static FRAMEBUFFER_BIOS: Once<Mutex<super::framebuffer::FramebufferWriter<u8>>> = Once::new();

#[cfg(target_os = "uefi")]
pub fn init(config: &FullereneFramebufferConfig) {
    petroleum::serial::serial_log(format_args!(
        "Graphics: Initializing UEFI framebuffer: {}x{}, stride: {}, pixel_format: {:?}\n",
        config.width, config.height, config.stride, config.pixel_format
    ));
    let writer = FramebufferWriter::<u32>::new(super::framebuffer::FramebufferInfo::new(config));
    WRITER_UEFI.call_once(|| Mutex::new(Box::new(writer.clone())));
    FRAMEBUFFER_UEFI.call_once(|| Mutex::new(super::framebuffer::UefiFramebuffer::Uefi32(writer)));
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
        FRAMEBUFFER_UEFI.call_once(|| {
            Mutex::new(super::framebuffer::UefiFramebuffer::Vga8(writer))
        });
    }

    #[cfg(not(target_os = "uefi"))]
    {
        WRITER_BIOS.call_once(|| Mutex::new(Box::new(writer.clone())));
        FRAMEBUFFER_BIOS.call_once(|| Mutex::new(writer));
    }
}

// All VGA setup is handled by petroleum's init_vga_graphics

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    #[cfg(target_os = "uefi")]
    {
        if let Some(writer) = WRITER_UEFI.get() {
            let mut writer = writer.lock();
            writer.write_fmt(args).ok();
        }
    }
    #[cfg(not(target_os = "uefi"))]
    {
        if let Some(writer) = WRITER_BIOS.get() {
            let mut writer = writer.lock();
            writer.write_fmt(args).ok();
        }
    }
    // Also output to VGA text buffer for reliable visibility
    if let Some(vga) = crate::vga::VGA_BUFFER.get() {
        let mut vga_writer = vga.lock();
        vga_writer.write_fmt(args).ok();
        vga_writer.update_cursor();
    }
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::graphics::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}
