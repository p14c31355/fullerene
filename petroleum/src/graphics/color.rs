// Use embedded_graphics features needed for drawing macros
use embedded_graphics::pixelcolor::*;

/// Unified color scheme and utilities for petroleum graphics modules
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

pub fn rgb_pixel(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

pub fn u32_to_rgb888(color: u32) -> Rgb888 {
    Rgb888::new(
        ((color >> 16) & 0xFF) as u8,
        ((color >> 8) & 0xFF) as u8,
        (color & 0xFF) as u8,
    )
}

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
        use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle};
        let rect = Rectangle::new(embedded_graphics::geometry::Point::new($x, $y), embedded_graphics::geometry::Size::new($w, $h));
        let style = PrimitiveStyleBuilder::new()
            .fill_color($crate::graphics::color::u32_to_rgb888($color))
            .build();
        rect.into_styled(style).draw($writer).ok();
    }};
}

#[macro_export]
macro_rules! draw_border_rect {
    ($writer:expr, $x:expr, $y:expr, $w:expr, $h:expr, $fill_color:expr, $stroke_color:expr, $stroke_width:expr) => {{
        use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle};
        let rect = Rectangle::new(embedded_graphics::geometry::Point::new($x, $y), embedded_graphics::geometry::Size::new($w, $h));
        let style = PrimitiveStyleBuilder::new()
            .fill_color(crate::graphics::color::u32_to_rgb888($fill_color))
            .stroke_color(crate::graphics::color::u32_to_rgb888($stroke_color))
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
