/// A safe, lifetime-constrained wrapper around framebuffer pixel data.
///
/// Provides read/write access to the framebuffer while constraining the
/// lifetime to the duration of a `with_framebuffer` closure call, preventing
/// `&'static mut` aliasing bugs.
pub struct FramebufferGuard<'a> {
    pixels: &'a mut [u32],
    width: u32,
    height: u32,
    stride: u32,
}

impl<'a> FramebufferGuard<'a> {
    /// Build a guard after validating the framebuffer layout.
    ///
    /// `stride` is expressed in pixels and may be larger than `width` when
    /// scan lines include padding. The backing slice must cover every scan
    /// line described by `stride` and `height`.
    pub fn try_new(pixels: &'a mut [u32], width: u32, height: u32, stride: u32) -> Option<Self> {
        let required_pixels = usize::try_from(stride).ok()?.checked_mul(height as usize)?;
        if width == 0 || height == 0 || stride < width || pixels.len() < required_pixels {
            return None;
        }

        Some(Self {
            pixels,
            width,
            height,
            stride,
        })
    }

    pub fn pixels(&self) -> &[u32] {
        self.pixels
    }

    pub fn pixels_mut(&mut self) -> &mut [u32] {
        self.pixels
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn stride(&self) -> u32 {
        self.stride
    }
}

#[cfg(test)]
mod framebuffer_guard_tests {
    use super::FramebufferGuard;

    #[test]
    fn exposes_validated_metadata_and_mutable_pixels() {
        let mut pixels = [0u32; 12];
        let mut guard = FramebufferGuard::try_new(&mut pixels, 3, 2, 4).unwrap();

        assert_eq!(guard.width(), 3);
        assert_eq!(guard.height(), 2);
        assert_eq!(guard.stride(), 4);
        assert_eq!(guard.pixels().len(), 8);
        guard.pixels_mut()[5] = 0x00ab_cdef;
        assert_eq!(guard.pixels()[5], 0x00ab_cdef);
    }

    #[test]
    fn rejects_invalid_layouts() {
        let mut pixels = [0u32; 8];

        assert!(FramebufferGuard::try_new(&mut pixels, 0, 2, 4).is_none());
        assert!(FramebufferGuard::try_new(&mut pixels, 5, 2, 4).is_none());
        assert!(FramebufferGuard::try_new(&mut pixels, 4, 3, 4).is_none());
    }
}

/// Renderer trait provides a generic interface for 2D graphics operations.
pub trait Renderer {
    fn draw_pixel(&mut self, x: i32, y: i32, color: u32);
    fn draw_rect(&mut self, x: i32, y: i32, width: u32, height: u32, color: u32);
    fn draw_text(&mut self, x: i32, y: i32, text: &str, color: u32);
    fn clear(&mut self, color: u32);
    fn get_resolution(&self) -> (u32, u32);
    fn present(&mut self) {}
}

/// Text console trait.
pub trait Console: core::fmt::Write {
    fn write_char(&mut self, c: char, color: u32);
    fn set_color(&mut self, color: u32);
    fn clear(&mut self);
    fn set_cursor(&mut self, x: usize, y: usize);
    fn scroll(&mut self);
}

pub mod boot_screen;
pub mod color;
pub mod constants;
pub mod framebuffer;
pub mod framebuffer_mapper;
pub mod registers;
pub mod setup;
pub mod text;
pub mod uefi;

pub use color::*;
pub use constants::*;
// VGA graphics modes
pub use framebuffer::UefiFramebufferWriter;
pub use framebuffer::*;
pub use setup::{
    detect_and_init_vga_graphics, detect_cirrus_vga, init_vga_graphics, init_vga_text_mode,
    setup_cirrus_vga_mode,
};
pub use text::{Color, ColorCode, ScreenChar, TextBufferOperations};
