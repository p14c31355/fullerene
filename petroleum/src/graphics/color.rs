use embedded_graphics::mono_font::{MonoTextStyle, ascii::FONT_6X10};
use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle};
use embedded_graphics::{geometry::Point, pixelcolor::Rgb888, prelude::*};

use core::marker::{Send, Sync};
use core::ptr::{read_volatile, write_volatile};

use crate::common::{EfiGraphicsPixelFormat, FullereneFramebufferConfig, VgaFramebufferConfig};
use spin::{Mutex, Once};

// --- FramebufferInfo ---
#[derive(Clone, Copy)]
pub struct FramebufferInfo {
    pub address: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_format: Option<EfiGraphicsPixelFormat>,
    pub colors: ColorScheme,
}

impl FramebufferInfo {
    pub fn width_or_stride(&self) -> u32 {
        #[cfg(target_os = "uefi")]
        {
            let stride_bytes = self.stride as u64 * self.bytes_per_pixel() as u64;
            stride_bytes
                .try_into()
                .expect("Stride in bytes exceeds u32::MAX")
        }
        #[cfg(not(target_os = "uefi"))]
        {
            self.width
        }
    }

    pub fn calculate_offset(&self, x: u32, y: u32) -> usize {
        #[cfg(target_os = "uefi")]
        {
            ((y as u64 * self.stride as u64 + x as u64) * self.bytes_per_pixel() as u64) as usize
        }
        #[cfg(not(target_os = "uefi"))]
        {
            ((y * self.width + x) * 1) as usize
        } // 1 byte per pixel for VGA
    }

    pub fn bytes_per_pixel(&self) -> u32 {
        match self.pixel_format {
            Some(EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor)
            | Some(EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor)
            | Some(EfiGraphicsPixelFormat::PixelBitMask) => 4,
            Some(EfiGraphicsPixelFormat::PixelBltOnly)
            | Some(EfiGraphicsPixelFormat::PixelFormatMax) => 0, // Invalid for direct access
            None => 1, // VGA
        }
    }
}

#[cfg(target_os = "uefi")]
impl FramebufferInfo {
    pub fn new(fb_config: &FullereneFramebufferConfig) -> Self {
        Self {
            address: fb_config.address + (crate::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64), // Assuming this path is correct in petroleum
            width: fb_config.width,
            height: fb_config.height,
            stride: fb_config.stride,
            pixel_format: Some(fb_config.pixel_format),
            colors: ColorScheme::UEFI_GREEN_ON_BLACK,
        }
    }
}

impl FramebufferInfo {
    pub fn new_vga(config: &VgaFramebufferConfig) -> Self {
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

// --- ColorScheme ---
#[derive(Clone, Copy)]
pub struct ColorScheme {
    pub fg: u32,
    pub bg: u32,
}

impl ColorScheme {
    pub const UEFI_GREEN_ON_BLACK: Self = Self {
        fg: 0x00FF00u32,
        bg: 0x000000u32,
    };
    pub const VGA_GREEN_ON_BLACK: Self = Self {
        fg: 0x02u32,
        bg: 0x00u32,
    };
}

// --- PixelType Trait ---
// Generic pixel type trait for type safety
pub trait PixelType: Copy + Send + Sync {
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

// --- VGA Color Index ---
/// Convert RGB888 color to VGA palette index (8-bit indexed color)
/// This is a simple approximation that maps common colors to their closest VGA equivalents
pub fn vga_color_index(r: u8, g: u8, b: u8) -> u32 {
    // Standard 16-color EGA/VGA palette with their approximate RGB values.
    const PALETTE: [(u8, u8, u8, u32); 16] = [
        (0, 0, 0, 0),        // Black
        (0, 0, 170, 1),      // Blue
        (0, 170, 0, 2),      // Green
        (0, 170, 170, 3),    // Cyan
        (170, 0, 0, 4),      // Red
        (170, 0, 170, 5),    // Magenta
        (170, 85, 0, 6),     // Brown
        (170, 170, 170, 7),  // Light Gray
        (85, 85, 85, 8),     // Dark Gray
        (85, 85, 255, 9),    // Light Blue
        (85, 255, 85, 10),   // Light Green
        (85, 255, 255, 11),  // Light Cyan
        (255, 85, 85, 12),   // Light Red
        (255, 85, 255, 13),  // Light Magenta
        (255, 255, 85, 14),  // Yellow
        (255, 255, 255, 15), // White
    ];

    let mut min_dist_sq = u32::MAX;
    let mut best_index = 0;

    for &(pr, pg, pb, index) in &PALETTE {
        let dr = r as i32 - pr as i32;
        let dg = g as i32 - pg as i32;
        let db = b as i32 - pb as i32;
        let dist_sq = (dr * dr + dg * dg + db * db) as u32;

        if dist_sq < min_dist_sq {
            min_dist_sq = dist_sq;
            best_index = index;
        }
    }

    best_index
}

// --- SimpleFramebuffer ---
/// Global simple framebuffer config (Redox vesad-style)
pub static SIMPLE_FRAMEBUFFER_CONFIG: Once<spin::Mutex<Option<SimpleFramebufferConfig>>> =
    Once::new();

/// Simple framebuffer config for recreation
#[derive(Clone, Copy)]
pub struct SimpleFramebufferConfig {
    pub base_addr: usize,
    pub width: usize,
    pub height: usize,
    pub stride: usize, // bytes per row
    pub bytes_per_pixel: usize,
}

/// Get the simple framebuffer instance (creates it from config if needed)
pub fn get_simple_framebuffer() -> Option<SimpleFramebuffer> {
    SIMPLE_FRAMEBUFFER_CONFIG.get().and_then(|config_mutex| {
        let config = config_mutex.lock();
        config.as_ref().map(|cfg| SimpleFramebuffer::new(*cfg))
    })
}

/// Initialize the simple framebuffer config
pub fn init_simple_framebuffer_config(config: SimpleFramebufferConfig) {
    SIMPLE_FRAMEBUFFER_CONFIG.call_once(|| Mutex::new(Some(config)));
}

/// Simple Framebuffer struct for direct MMIO pixel manipulation (Redox vesad-style)
pub struct SimpleFramebuffer {
    pub base: usize, // Use usize instead of raw pointer to avoid Send/Sync issues
    pub width: usize,
    pub height: usize,
    pub stride: usize, // bytes per row
    pub bytes_per_pixel: usize,
}

impl SimpleFramebuffer {
    /// Create a new framebuffer from GOP config
    pub fn new(config: SimpleFramebufferConfig) -> Self {
        Self {
            base: config.base_addr,
            width: config.width,
            height: config.height,
            stride: config.stride,
            bytes_per_pixel: config.bytes_per_pixel,
        }
    }

    /// Clear the entire framebuffer
    pub fn clear(&mut self, color: u32) {
        let color_bytes = color.to_le_bytes();
        for y in 0..self.height {
            let row_base = self.base + y * self.stride;
            for x in 0..self.width {
                let offset = x * self.bytes_per_pixel;
                let pixel_addr = (row_base + offset) as *mut u8;

                // Check that the calculated pixel_addr is within the valid framebuffer memory region
                let pixel_addr_usize = pixel_addr as usize;
                if pixel_addr_usize < self.base
                    || (pixel_addr_usize + self.bytes_per_pixel)
                        > (self.base + self.height * self.stride)
                {
                    continue;
                }

                unsafe {
                    for i in 0..self.bytes_per_pixel {
                        if i < color_bytes.len() {
                            write_volatile(pixel_addr.add(i), color_bytes[i]);
                        }
                    }
                }
            }
        }
    }

    /// Draw a single pixel (orbclient-style)
    pub fn draw_pixel(&mut self, x: usize, y: usize, color: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let row_base = self.base + y * self.stride;
        let offset = x * self.bytes_per_pixel;
        let pixel_addr = (row_base + offset) as *mut u8;

        // Check that the calculated pixel_addr is within the valid framebuffer memory region
        let pixel_addr_usize = pixel_addr as usize;
        if pixel_addr_usize < self.base
            || (pixel_addr_usize + self.bytes_per_pixel) > (self.base + self.height * self.stride)
        {
            return;
        }

        unsafe {
            let color_bytes = color.to_le_bytes();
            for i in 0..self.bytes_per_pixel {
                if i < color_bytes.len() {
                    write_volatile(pixel_addr.add(i), color_bytes[i]);
                }
            }
        }
    }

    /// Draw a filled rectangle (orbclient-style)
    pub fn draw_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: u32) {
        for dy in 0..height {
            if y + dy >= self.height {
                break;
            }
            for dx in 0..width {
                if x + dx >= self.width {
                    break;
                }
                self.draw_pixel(x + dx, y + dy, color);
            }
        }
    }

    /// Read a pixel (for reference, though not used in Redox)
    pub fn get_pixel(&self, x: usize, y: usize) -> u32 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        let row_base = self.base + y * self.stride;
        let offset = x * self.bytes_per_pixel;
        let pixel_addr = (row_base + offset) as *const u32;
        unsafe { read_volatile(pixel_addr) }
    }

    /// Get framebuffer dimensions
    pub fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }
}

// --- Button and Drawing Macros ---
// UI Color constants for desktop graphics
pub const COLOR_LIGHT_GRAY: u32 = 0xE0E0E0;
pub const COLOR_BLACK: u32 = 0x000000;
pub const COLOR_DARK_GRAY: u32 = 0xA0A0A0;
pub const COLOR_WHITE: u32 = 0xFFFFFF;
pub const COLOR_LIGHT_BLUE: u32 = 0xADD8E6;
pub const COLOR_TASKBAR: u32 = 0xC0C0C0;
pub const COLOR_WINDOW_BG: u32 = 0xF8F8F8;

// GUI Drawing macros to reduce repetitive code - kept simple for no_std compatibility
#[macro_export]
macro_rules! create_button {
    ($x:expr, $y:expr, $width:expr, $height:expr, $text:expr, $bg:expr, $text_color:expr) => {
        Button::new($x, $y, $width, $height, $text).with_colors($bg, $text_color)
    };
}

// Simplified drawing macros for common UI elements
#[macro_export]
macro_rules! draw_filled_rect {
    ($writer:expr, $x:expr, $y:expr, $w:expr, $h:expr, $color:expr) => {{
        let rect = Rectangle::new(
            embedded_graphics::geometry::Point::new($x, $y),
            embedded_graphics::geometry::Size::new($w, $h),
        );
        let style = PrimitiveStyleBuilder::new()
            .fill_color($crate::graphics::color::u32_to_rgb888($color))
            .build();
        rect.into_styled(style).draw($writer).ok();
    }};
}

#[macro_export]
macro_rules! draw_border_rect {
    ($writer:expr, $x:expr, $y:expr, $w:expr, $h:expr, $fill_color:expr, $stroke_color:expr, $stroke_width:expr) => {{
        let rect = Rectangle::new(
            embedded_graphics::geometry::Point::new($x, $y),
            embedded_graphics::geometry::Size::new($w, $h),
        );
        let style = PrimitiveStyleBuilder::new()
            .fill_color($crate::graphics::color::u32_to_rgb888($fill_color))
            .stroke_color($crate::graphics::color::u32_to_rgb888($stroke_color))
            .stroke_width($stroke_width)
            .build();
        rect.into_styled(style).draw($writer).ok();
    }};
}

// Simple GUI element definitions (basic structs without complex trait impls)
pub struct Button {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub text: alloc::string::String,
    pub bg_color: u32,
    pub text_color: u32,
}

impl Button {
    pub fn new(x: u32, y: u32, width: u32, height: u32, text: &str) -> Self {
        Self {
            x,
            y,
            width,
            height,
            text: alloc::string::ToString::to_string(text),
            bg_color: COLOR_LIGHT_GRAY,
            text_color: COLOR_BLACK,
        }
    }

    pub fn with_colors(mut self, bg: u32, text_color: u32) -> Self {
        self.bg_color = bg;
        self.text_color = text_color;
        self
    }

    pub fn contains_point(&self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

// Text width calculation for monospaced font
pub fn calc_text_width(text: &str) -> i32 {
    (text.len() * 6) as i32
}

pub fn grayscale_intensity(color: Rgb888) -> u32 {
    ((color.r() as u32 * 77 + color.g() as u32 * 150 + color.b() as u32 * 29) / 256).min(255)
}

// u32 to Rgb888 conversion function
pub fn u32_to_rgb888(color: u32) -> Rgb888 {
    Rgb888::new(
        ((color >> 16) & 0xFF) as u8,
        ((color >> 8) & 0xFF) as u8,
        (color & 0xFF) as u8,
    )
}

// Define rgb_pixel here as it's used within this file.
pub fn rgb_pixel(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}
