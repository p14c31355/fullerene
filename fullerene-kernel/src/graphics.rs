// PortWriter, VgaPortOps, and related macros are now in petroleum crate
use petroleum::graphics::init_vga_graphics;
use petroleum::common::*;

// Helper functions for color operations

use alloc::boxed::Box; // Import Box
use core::fmt::{self, Write};
use core::marker::{Send, Sync};
use petroleum::common::VgaFramebufferConfig;
use petroleum::common::{EfiGraphicsPixelFormat, FullereneFramebufferConfig}; // Import missing types
use petroleum::{clear_buffer_pixels, scroll_buffer_pixels};
use spin::{Mutex, Once};
use x86_64::instructions::port::Port;

use crate::font::FONT_8X8;

// A simple 8x8 PC screen font (Code Page 437).
static FONT: [[u8; 8]; 128] = FONT_8X8;

// Helper struct to reduce position update boilerplate
struct TextPosition {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl TextPosition {
    fn new(fb: &impl FramebufferLike) -> Self {
        let (x, y) = fb.get_position();
        Self {
            x,
            y,
            width: fb.get_width(),
            height: fb.get_height(),
        }
    }

    fn new_line(&mut self, fb: &mut impl FramebufferLike) {
        self.y += 4; // Scaled font height
        self.x = 0;
        if self.y + 4 > self.height {
            fb.scroll_up();
            self.y -= 4;
        }
        fb.set_position(self.x, self.y);
    }

    fn advance_char(&mut self, fb: &mut impl FramebufferLike) {
        self.x += 4; // Scaled font width
        if self.x + 4 > self.width {
            self.new_line(fb);
        } else {
            fb.set_position(self.x, self.y);
        }
    }
}

/// Generic function to draw a character on any framebuffer using the full font data
fn draw_char(fb: &impl FramebufferLike, c: char, x: u32, y: u32) {
    let char_idx = c as usize;
    if char_idx >= 128 {
        return; // Character not in our font
    }

    let font_char = &FONT[char_idx];
    let fg = fb.get_fg_color();
    let bg = fb.get_bg_color();

    for row in 0..8u32 {  // Full height of font character
        let byte = font_char[row as usize];
        for col in 0..8u32 {  // Full width of font character
            let bit_position = col as usize; // Leftmost bit corresponds to leftmost pixel
            let color = if (byte & (1 << (7 - bit_position))) != 0 { fg } else { bg };
            fb.put_pixel(x + col, y + row, color);
        }
    }
}

// Updated write_text to ensure it uses the font data properly
fn write_text<W: FramebufferLike>(writer: &mut W, s: &str) -> core::fmt::Result {
    let mut pos = TextPosition::new(writer);

    for c in s.chars() {
        if c == '\n' {
            pos.new_line(writer);
        } else {
            draw_char(writer, c, pos.x, pos.y);
            pos.advance_char(writer);
        }
    }
    Ok(())
}

/// Generic framebuffer operations (shared functions in petroleum crate)

struct ColorScheme {
    fg: u32,
    bg: u32,
}

impl ColorScheme {
    const UEFI_GREEN_ON_BLACK: Self = Self {
        fg: 0x00FF00u32,
        bg: 0x000000u32,
    };
    const VGA_GREEN_ON_BLACK: Self = Self {
        fg: 0x02u32,
        bg: 0x00u32,
    };
}

struct FramebufferInfo {
    address: u64,
    width: u32,
    height: u32,
    stride: u32,
    pixel_format: Option<EfiGraphicsPixelFormat>,
    colors: ColorScheme,
}

impl FramebufferInfo {
    fn width_or_stride(&self) -> u32 {
        #[cfg(target_os = "uefi")]
        {
            self.stride
        }
        #[cfg(not(target_os = "uefi"))]
        {
            self.width
        }
    }

    fn calculate_offset(&self, x: u32, y: u32) -> usize {
        #[cfg(target_os = "uefi")]
        {
            ((y * self.stride + x) * self.bytes_per_pixel()) as usize
        }
        #[cfg(not(target_os = "uefi"))]
        {
            ((y * self.width + x) * 1) as usize
        } // 1 byte per pixel for VGA
    }

    fn bytes_per_pixel(&self) -> u32 {
        match self.pixel_format {
            Some(EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor)
            | Some(EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor) => 4,
            Some(_) => panic!("Unsupported pixel format"),
            None => 1, // VGA
        }
    }
}

#[cfg(target_os = "uefi")]
impl FramebufferInfo {
    fn new(fb_config: &FullereneFramebufferConfig) -> Self {
        Self {
            address: fb_config.address,
            width: fb_config.width,
            height: fb_config.height,
            stride: fb_config.stride,
            pixel_format: Some(fb_config.pixel_format),
            colors: ColorScheme::UEFI_GREEN_ON_BLACK,
        }
    }
}

impl FramebufferInfo {
    fn new_vga(config: &VgaFramebufferConfig) -> Self {
        Self {
            address: config.address,
            width: config.width,
            height: config.height,
            stride: config.width,
            pixel_format: None,
            colors: ColorScheme::VGA_GREEN_ON_BLACK,
        }
    }
}

// Generic pixel type trait for type safety
trait PixelType: Copy {
    fn bytes_per_pixel() -> u32;
    fn from_u32(color: u32) -> Self;
    fn to_generic(color: u32) -> u32;
}

impl PixelType for u32 {
    fn bytes_per_pixel() -> u32 {
        4
    }
    fn from_u32(color: u32) -> Self {
        color
    }
    fn to_generic(color: u32) -> u32 {
        color
    }
}

impl PixelType for u8 {
    fn bytes_per_pixel() -> u32 {
        1
    }
    fn from_u32(color: u32) -> Self {
        color as u8
    }
    fn to_generic(color: u32) -> u32 {
        color & 0xFF
    }
}

trait FramebufferLike {
    fn put_pixel(&self, x: u32, y: u32, color: u32);
    fn clear_screen(&self);
    fn get_width(&self) -> u32;
    fn get_height(&self) -> u32;
    fn get_fg_color(&self) -> u32;
    fn get_bg_color(&self) -> u32;
    fn set_position(&mut self, x: u32, y: u32);
    fn get_position(&self) -> (u32, u32);
    fn scroll_up(&self);
}

struct FramebufferWriter<T: PixelType> {
    info: FramebufferInfo,
    x_pos: u32,
    y_pos: u32,
    _phantom: core::marker::PhantomData<T>,
}

impl<T: PixelType> FramebufferWriter<T> {
    fn new(info: FramebufferInfo) -> Self {
        Self {
            info,
            x_pos: 0,
            y_pos: 0,
            _phantom: core::marker::PhantomData,
        }
    }

    fn scroll_up(&self) {
        unsafe {
            scroll_buffer_pixels::<T>(
                self.info.address,
                self.info.width_or_stride(),
                self.info.height,
                T::from_u32(self.info.colors.bg),
            );
        }
    }
}

impl<T: PixelType> FramebufferLike for FramebufferWriter<T> {
    fn put_pixel(&self, x: u32, y: u32, color: u32) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }

        let offset = self.info.calculate_offset(x, y);
        unsafe {
            let fb_ptr = self.info.address as *mut u8;
            let pixel_ptr = fb_ptr.add(offset) as *mut T;
            *pixel_ptr = T::from_u32(color);
        }
    }

    fn clear_screen(&self) {
        unsafe {
            clear_buffer_pixels::<T>(
                self.info.address,
                self.info.width_or_stride(),
                self.info.height,
                T::from_u32(self.info.colors.bg),
            );
        }
    }

    fn get_width(&self) -> u32 {
        self.info.width
    }
    fn get_height(&self) -> u32 {
        self.info.height
    }
    fn get_fg_color(&self) -> u32 {
        self.info.colors.fg
    }
    fn get_bg_color(&self) -> u32 {
        self.info.colors.bg
    }

    fn set_position(&mut self, x: u32, y: u32) {
        self.x_pos = x;
        self.y_pos = y;
    }

    fn get_position(&self) -> (u32, u32) {
        (self.x_pos, self.y_pos)
    }

    fn scroll_up(&self) {
        unsafe {
            petroleum::scroll_buffer_pixels::<T>(
                self.info.address,
                self.info.width_or_stride(),
                self.info.height,
                T::from_u32(self.info.colors.bg),
            );
        }
    }
}

// Convenience type aliases
type UefiFramebufferWriter = FramebufferWriter<u32>;
type VgaFramebufferWriter = FramebufferWriter<u8>;

impl<T: PixelType> core::fmt::Write for FramebufferWriter<T> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        write_text(self, s)
    }
}

#[cfg(target_os = "uefi")]
pub static WRITER_UEFI: Once<Mutex<Box<dyn core::fmt::Write + Send + Sync>>> = Once::new();

#[cfg(not(target_os = "uefi"))]
pub static WRITER_BIOS: Once<Mutex<Box<dyn core::fmt::Write + Send + Sync>>> = Once::new();

#[cfg(target_os = "uefi")]
pub fn init(config: &FullereneFramebufferConfig) {
    let writer = FramebufferWriter::<u32>::new(FramebufferInfo::new(config));
    writer.clear_screen();
    WRITER_UEFI.call_once(|| Mutex::new(Box::new(writer)));
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
    WRITER_UEFI.call_once(|| Mutex::new(Box::new(writer)));
    #[cfg(not(target_os = "uefi"))]
    WRITER_BIOS.call_once(|| Mutex::new(Box::new(writer)));
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
