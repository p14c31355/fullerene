//! Allocation-free boot splash rendering for the GOP framebuffer.
//!
//! The bootloader and kernel both use this renderer with the framebuffer's
//! already-established direct mapping. Keeping one write path avoids creating
//! cache-incoherent aliases for scan-out memory on physical machines.

use crate::common::{EfiGraphicsPixelFormat, FullereneFramebufferConfig};

/// Number of kernel initialization stages shown in the progress bar.
pub const KERNEL_STAGE_COUNT: u8 = 15;

/// A validated, directly accessible 32-bpp GOP framebuffer.
#[derive(Clone, Copy)]
pub struct BootFramebuffer {
    address: u64,
    width: u32,
    height: u32,
    stride_pixels: u32,
    pixel_format: EfiGraphicsPixelFormat,
}

impl BootFramebuffer {
    /// Validate raw framebuffer parameters and construct a boot renderer.
    pub fn new(
        address: u64,
        width: u32,
        height: u32,
        stride_bytes: u32,
        bpp: u32,
        pixel_format: u32,
    ) -> Option<Self> {
        if address == 0
            || !(160..=16_384).contains(&width)
            || !(120..=16_384).contains(&height)
            || bpp != 32
            || stride_bytes < width.checked_mul(4)?
            || stride_bytes % 4 != 0
        {
            return None;
        }
        let pixel_format = match pixel_format {
            0 => EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
            1 => EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor,
            _ => return None,
        };
        let stride_pixels = stride_bytes / 4;
        let _ = stride_pixels.checked_mul(height)?;
        Some(Self {
            address,
            width,
            height,
            stride_pixels,
            pixel_format,
        })
    }

    pub fn from_config(config: FullereneFramebufferConfig) -> Option<Self> {
        Self::new(
            config.address,
            config.width,
            config.height,
            config.stride,
            config.bpp,
            config.pixel_format as u32,
        )
    }

    /// Draw the splash panel and the current initialization stage.
    ///
    /// # Safety
    /// `address` must remain mapped and writable for the full framebuffer.
    pub unsafe fn draw_stage(&self, completed: u8, total: u8, label: &[u8]) {
        if total == 0 {
            return;
        }

        let margin = (self.width.min(self.height) / 20).clamp(12, 40);
        let panel_width = self.width.saturating_sub(margin * 2).min(760);
        let panel_height = if self.height >= 360 { 180 } else { 132 };
        let panel_height = panel_height.min(self.height.saturating_sub(margin * 2));
        if panel_width < 120 || panel_height < 100 {
            return;
        }
        let panel_x = (self.width - panel_width) / 2;
        let panel_y = (self.height - panel_height) / 2;

        let panel = self.rgb(31, 35, 42);
        let border = self.rgb(88, 94, 105);
        let text = self.rgb(244, 246, 248);
        let muted = self.rgb(94, 100, 111);
        let red = self.rgb(233, 69, 96);
        let blue = self.rgb(54, 132, 246);
        let magenta = self.rgb(210, 71, 198);

        unsafe {
            self.fill_rect(panel_x, panel_y, panel_width, panel_height, border);
            self.fill_rect(
                panel_x + 2,
                panel_y + 2,
                panel_width.saturating_sub(4),
                panel_height.saturating_sub(4),
                panel,
            );

            // Preserve the existing red / blue / magenta diagnostic language,
            // but make it a deliberate part of the boot splash.
            let accent_width = panel_width.saturating_sub(4);
            let third = accent_width / 3;
            self.fill_rect(panel_x + 2, panel_y + 2, third, 4, red);
            self.fill_rect(panel_x + 2 + third, panel_y + 2, third, 4, blue);
            self.fill_rect(
                panel_x + 2 + third * 2,
                panel_y + 2,
                accent_width.saturating_sub(third * 2),
                4,
                magenta,
            );

            let title_scale = if self.width >= 640 { 3 } else { 2 };
            self.draw_text_centered(b"FULLERENE OS", panel_y + 22, title_scale, text);

            let label_scale = if self.width >= 480 { 2 } else { 1 };
            let label_y = panel_y + if panel_height >= 160 { 82 } else { 62 };
            self.draw_text_centered(label, label_y, label_scale, text);

            let bar_x = panel_x + 20;
            let bar_width = panel_width.saturating_sub(40);
            let bar_y = panel_y + panel_height.saturating_sub(32);
            let gap = 2u32;
            let segments = u32::from(total);
            let gaps_width = gap.saturating_mul(segments.saturating_sub(1));
            let segment_width = bar_width.saturating_sub(gaps_width) / segments;
            if segment_width != 0 {
                for index in 0..segments {
                    let color = if index < u32::from(completed.min(total)) {
                        match index % 3 {
                            0 => red,
                            1 => blue,
                            _ => magenta,
                        }
                    } else {
                        muted
                    };
                    self.fill_rect(
                        bar_x + index * (segment_width + gap),
                        bar_y,
                        segment_width,
                        10,
                        color,
                    );
                }
            }
        }
        unsafe { core::arch::x86_64::_mm_sfence() };
    }

    fn rgb(&self, red: u8, green: u8, blue: u8) -> u32 {
        match self.pixel_format {
            // Byte order in memory is R, G, B, reserved.
            EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor => {
                u32::from(red) | (u32::from(green) << 8) | (u32::from(blue) << 16)
            }
            // Byte order in memory is B, G, R, reserved.
            EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor => {
                u32::from(blue) | (u32::from(green) << 8) | (u32::from(red) << 16)
            }
            _ => 0,
        }
    }

    unsafe fn fill_rect(&self, x: u32, y: u32, width: u32, height: u32, color: u32) {
        let x_end = x.saturating_add(width).min(self.width);
        let y_end = y.saturating_add(height).min(self.height);
        let base = self.address as *mut u32;
        for py in y..y_end {
            let row = py as usize * self.stride_pixels as usize;
            for px in x..x_end {
                unsafe { core::ptr::write_volatile(base.add(row + px as usize), color) };
            }
        }
    }

    unsafe fn draw_text_centered(&self, text: &[u8], y: u32, scale: u32, color: u32) {
        let width = text_width(text, scale);
        let x = self.width.saturating_sub(width) / 2;
        unsafe { self.draw_text(x, y, text, scale, color) };
    }

    unsafe fn draw_text(&self, mut x: u32, y: u32, text: &[u8], scale: u32, color: u32) {
        for &byte in text {
            let rows = glyph(byte.to_ascii_uppercase());
            for (gy, bits) in rows.iter().copied().enumerate() {
                for gx in 0..5u32 {
                    if bits & (1 << (4 - gx)) != 0 {
                        unsafe {
                            self.fill_rect(
                                x.saturating_add(gx.saturating_mul(scale)),
                                y.saturating_add((gy as u32).saturating_mul(scale)),
                                scale,
                                scale,
                                color,
                            )
                        };
                    }
                }
            }
            x = x.saturating_add(6 * scale);
        }
    }
}

fn text_width(text: &[u8], scale: u32) -> u32 {
    (text.len() as u32)
        .saturating_mul(6 * scale)
        .saturating_sub(scale)
}

/// Compact 5x7 uppercase font. Bits 4..0 are the left-to-right pixels.
fn glyph(byte: u8) -> [u8; 7] {
    match byte {
        b'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        b'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        b'C' => [
            0b01111, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b01111,
        ],
        b'D' => [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        b'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        b'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        b'G' => [
            0b01111, 0b10000, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111,
        ],
        b'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        b'I' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111,
        ],
        b'J' => [
            0b00111, 0b00010, 0b00010, 0b00010, 0b10010, 0b10010, 0b01100,
        ],
        b'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        b'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        b'M' => [
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
        b'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        b'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        b'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        b'Q' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
        b'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        b'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        b'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        b'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        b'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        b'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010,
        ],
        b'X' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
        b'Y' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        b'Z' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        b'0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        b'1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        b'2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        b'3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        b'4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        b'5' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110,
        ],
        b'6' => [
            0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        b'7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        b'8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        b'9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110,
        ],
        _ => [0; 7],
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn converts_both_gop_pixel_orders() {
        let rgb = BootFramebuffer::new(1, 320, 200, 1280, 32, 0).unwrap();
        let bgr = BootFramebuffer::new(1, 320, 200, 1280, 32, 1).unwrap();
        assert_eq!(rgb.rgb(0x12, 0x34, 0x56), 0x0056_3412);
        assert_eq!(bgr.rgb(0x12, 0x34, 0x56), 0x0012_3456);
    }

    #[test]
    fn draws_panel_text_and_all_progress_segments() {
        let mut pixels = std::vec![0u32; 320 * 200];
        let fb =
            BootFramebuffer::new(pixels.as_mut_ptr() as u64, 320, 200, 320 * 4, 32, 1).unwrap();
        unsafe { fb.draw_stage(KERNEL_STAGE_COUNT, KERNEL_STAGE_COUNT, b"GRAPHICS READY") };
        assert!(pixels.iter().filter(|&&pixel| pixel != 0).count() > 10_000);
        assert!(pixels.contains(&fb.rgb(233, 69, 96)));
        assert!(pixels.contains(&fb.rgb(54, 132, 246)));
        assert!(pixels.contains(&fb.rgb(210, 71, 198)));
    }
}
