//! Pre-rendered SVG icons — RGBA pixel data generated at build time.
//!
//! Each icon is a 64×64 RGBA pixel buffer embedded into the binary.
//! The Painter renders them via simple blit.

use crate::surface::Surface;

/// A pre-rendered 64×64 RGBA icon.
pub struct SvgIcon {
    pixels: &'static [u8], // 64*64*4 = 16384 bytes
}

impl SvgIcon {
    const fn from_rgba(data: &'static [u8]) -> Self {
        Self { pixels: data }
    }

    pub fn surface(&self) -> Surface {
        let mut s = Surface::new(64, 64, 0);
        let buf = s.pixels_mut();
        for (i, chunk) in self.pixels.chunks_exact(4).enumerate() {
            let r = chunk[0] as u32;
            let g = chunk[1] as u32;
            let b = chunk[2] as u32;
            let a = chunk[3] as u32;
            // Premultiplied alpha to straight alpha conversion, store as 0xRRGGBBAA
            let pixel = if a == 0 { 0 } else { (r << 16) | (g << 8) | b | (a << 24) };
            buf[i] = pixel;
        }
        s
    }
}

pub static ICON_SHELL: SvgIcon = SvgIcon::from_rgba(include_bytes!(concat!(env!("OUT_DIR"), "/icon_shell.rgba")));
pub static ICON_FILES: SvgIcon = SvgIcon::from_rgba(include_bytes!(concat!(env!("OUT_DIR"), "/icon_files.rgba")));
pub static ICON_SETTINGS: SvgIcon = SvgIcon::from_rgba(include_bytes!(concat!(env!("OUT_DIR"), "/icon_settings.rgba")));
pub static ICON_ABOUT: SvgIcon = SvgIcon::from_rgba(include_bytes!(concat!(env!("OUT_DIR"), "/icon_about.rgba")));
