use embedded_graphics::pixelcolor::*;

/// Unified color scheme and utilities to reduce duplication across graphics modules
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

pub fn grayscale_intensity(color: Rgb888) -> u32 {
    ((color.r() as u32 * 77 + color.g() as u32 * 150 + color.b() as u32 * 29) / 256).min(255)
}
