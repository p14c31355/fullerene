//! Allocation-free boot splash rendering for the GOP framebuffer.
//!
//! The bootloader and kernel both use this renderer with the framebuffer's
//! already-established direct mapping. Keeping one write path avoids creating
//! cache-incoherent aliases for scan-out memory on physical machines.

use crate::common::{EfiGraphicsPixelFormat, FullereneFramebufferConfig};
use sealant::{FramebufferRegion, Permissions};

/// Number of kernel initialization stages shown in the progress bar.
pub const KERNEL_STAGE_COUNT: u8 = 15;

/// A validated, directly accessible 32-bpp GOP framebuffer.
#[derive(Clone)]
pub struct BootFramebuffer {
    framebuffer: FramebufferRegion<'static>,
    address: u64,
    width: u32,
    height: u32,
    stride_pixels: u32,
    pixel_format: EfiGraphicsPixelFormat,
}

impl BootFramebuffer {
    /// Validate raw framebuffer parameters and construct a boot renderer.
    ///
    /// # Safety
    ///
    /// `address..address + stride_bytes * height` must be a mapped, writable
    /// framebuffer for the lifetime of the returned renderer.
    pub unsafe fn new(
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
        let len = (stride_bytes as usize).checked_mul(height as usize)?;
        let framebuffer = unsafe {
            FramebufferRegion::from_address(address as usize, len, Permissions::READ_WRITE).ok()?
        };
        Some(Self {
            framebuffer,
            address,
            width,
            height,
            stride_pixels,
            pixel_format,
        })
    }

    pub fn address(&self) -> u64 {
        self.address
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn stride_pixels(&self) -> u32 {
        self.stride_pixels
    }

    /// Construct a renderer from a firmware framebuffer configuration.
    ///
    /// # Safety
    ///
    /// The configuration's framebuffer range must remain mapped and writable.
    pub unsafe fn from_config(config: FullereneFramebufferConfig) -> Option<Self> {
        unsafe {
            Self::new(
                config.address,
                config.width,
                config.height,
                config.stride,
                config.bpp,
                config.pixel_format as u32,
            )
        }
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

        {
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

    fn fill_rect(&self, x: u32, y: u32, width: u32, height: u32, color: u32) {
        let x_end = x.saturating_add(width).min(self.width);
        let y_end = y.saturating_add(height).min(self.height);
        for py in y..y_end {
            let row = py as usize * self.stride_pixels as usize;
            for px in x..x_end {
                let _ = self
                    .framebuffer
                    .write_volatile_at((row + px as usize) * 4, color);
            }
        }
    }

    fn draw_text_centered(&self, text: &[u8], y: u32, scale: u32, color: u32) {
        let width = text_width(text, scale);
        let x = self.width.saturating_sub(width) / 2;
        self.draw_text(x, y, text, scale, color);
    }

    pub fn draw_text(&self, mut x: u32, y: u32, text: &[u8], scale: u32, color: u32) {
        for &byte in text {
            let rows = glyph(byte.to_ascii_uppercase());
            for (gy, bits) in rows.iter().copied().enumerate() {
                for gx in 0..5u32 {
                    if bits & (1 << (4 - gx)) != 0 {
                        let rx = x.saturating_add(gx.saturating_mul(scale));
                        let ry = y.saturating_add((gy as u32).saturating_mul(scale));
                        if rx < self.width && ry < self.height {
                            self.fill_rect(rx, ry, scale, scale, color);
                        }
                    }
                }
            }
            x = x.saturating_add(6 * scale);
        }
    }

    /// Draw the compact interrupt-safe font with a rational pixel scale.
    /// Fixed-point coordinates provide a 3/2 scale without floating-point
    /// work in the interrupt path.
    unsafe fn draw_text_scaled(
        &self,
        mut x: u32,
        y: u32,
        text: &[u8],
        scale_num: u32,
        scale_den: u32,
        color: u32,
    ) {
        if scale_num == 0 || scale_den == 0 {
            return;
        }

        for &byte in text {
            let rows = glyph(byte.to_ascii_uppercase());
            for (gy, bits) in rows.iter().copied().enumerate() {
                for gx in 0..5u32 {
                    if bits & (1 << (4 - gx)) != 0 {
                        let rx = x.saturating_add(gx.saturating_mul(scale_num) / scale_den);
                        let ry =
                            y.saturating_add((gy as u32).saturating_mul(scale_num) / scale_den);
                        let rx_next =
                            x.saturating_add((gx + 1).saturating_mul(scale_num) / scale_den);
                        let ry_next =
                            y.saturating_add((gy as u32 + 1).saturating_mul(scale_num) / scale_den);

                        if rx < self.width && ry < self.height {
                            self.fill_rect(
                                rx,
                                ry,
                                rx_next.saturating_sub(rx).max(1),
                                ry_next.saturating_sub(ry).max(1),
                                color,
                            );
                        }
                    }
                }
            }
            x = x.saturating_add(6 * scale_num / scale_den);
        }
    }

    /// Draw the Klog Live contents directly into an existing window's client
    /// area.
    ///
    /// This intentionally bypasses the normal compositor and allocators. It
    /// draws only the log text; it does not draw a title, border, or any other
    /// second window. It is
    /// used only by the kernel's timer path so the existing Klog Live window
    /// remains readable when the scheduler or compositor is blocked. The
    /// caller must ensure that the framebuffer mapping is valid for the
    /// duration of the write.
    pub unsafe fn draw_klog_live_surface(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        text: &[u8],
    ) {
        let x = x.min(self.width);
        let y = y.min(self.height);
        let width = width.min(self.width.saturating_sub(x));
        let height = height.min(self.height.saturating_sub(y));
        if width < 40 || height < 32 {
            return;
        }

        let panel = self.rgb(13, 13, 20);
        let body_fg = self.rgb(170, 221, 255);
        self.fill_rect(x, y, width, height, panel);

        // The fallback uses a compact 5x7 glyph. Use a fixed-point 3/2 scale
        // so the interrupt overlay is easier to read without the earlier 2x
        // diagnostic enlargement.
        const SCALE_NUM: u32 = 3;
        const SCALE_DEN: u32 = 2;
        const CELL_WIDTH: u32 = (6 * SCALE_NUM + SCALE_DEN - 1) / SCALE_DEN;
        const CELL_HEIGHT: u32 = (8 * SCALE_NUM + SCALE_DEN - 1) / SCALE_DEN;
        const BODY_OFFSET: u32 = (8 * SCALE_NUM + SCALE_DEN - 1) / SCALE_DEN;
        let max_cols = (width / CELL_WIDTH).min(100);
        let max_lines = (height.saturating_sub(BODY_OFFSET) / CELL_HEIGHT).min(29);
        if max_cols == 0 || max_lines == 0 {
            return;
        }
        if max_cols.saturating_mul(max_lines) > 100 * 29 {
            return;
        }

        unsafe {
            let header = b"--- KLog Live (auto-refresh) ---";
            let header_len = header.len().min(max_cols as usize);
            self.draw_text_scaled(x, y, &header[..header_len], SCALE_NUM, SCALE_DEN, body_fg);
        }

        // Count complete lines and skip older lines so the newest messages
        // remain visible without allocating or sorting in interrupt context.
        let mut line_count = 1u32;
        for &byte in text {
            if byte == b'\n' {
                line_count = line_count.saturating_add(1);
            }
        }
        let first_line = line_count.saturating_sub(max_lines);
        let mut line = 0u32;
        let mut col = 0u32;
        let body_y = y.saturating_add(BODY_OFFSET);
        for &byte in text {
            if byte == b'\n' {
                line = line.saturating_add(1);
                col = 0;
                continue;
            }
            if line < first_line {
                continue;
            }
            if line >= line_count || line - first_line >= max_lines {
                break;
            }
            if col < max_cols {
                let px = x.saturating_add(col.saturating_mul(CELL_WIDTH));
                let py = body_y.saturating_add((line - first_line).saturating_mul(CELL_HEIGHT));
                unsafe { self.draw_text_scaled(px, py, &[byte], SCALE_NUM, SCALE_DEN, body_fg) };
            }
            col = col.saturating_add(1);
        }
        unsafe { core::arch::x86_64::_mm_sfence() };
    }
}

fn text_width(text: &[u8], scale: u32) -> u32 {
    (text.len() as u32)
        .saturating_mul(6 * scale)
        .saturating_sub(scale)
}

/// Compact 5x7 uppercase font. Bits 4..0 are the left-to-right pixels.
const UPPER_GLYPHS: [[u8; 7]; 26] = [
    [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001], // A
    [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110], // B
    [0b01111, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b01111], // C
    [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110], // D
    [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111], // E
    [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000], // F
    [0b01111, 0b10000, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111], // G
    [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001], // H
    [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111], // I
    [0b00111, 0b00010, 0b00010, 0b00010, 0b10010, 0b10010, 0b01100], // J
    [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001], // K
    [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111], // L
    [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001], // M
    [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001], // N
    [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110], // O
    [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000], // P
    [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101], // Q
    [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001], // R
    [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110], // S
    [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100], // T
    [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110], // U
    [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100], // V
    [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010], // W
    [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001], // X
    [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100], // Y
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111], // Z
];
const DIGIT_GLYPHS: [[u8; 7]; 10] = [
    [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110], // 0
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110], // 1
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111], // 2
    [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110], // 3
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010], // 4
    [0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110], // 5
    [0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110], // 6
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000], // 7
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110], // 8
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110], // 9
];

fn glyph(byte: u8) -> [u8; 7] {
    if byte.is_ascii_uppercase() {
        UPPER_GLYPHS[(byte - b'A') as usize]
    } else if byte.is_ascii_digit() {
        DIGIT_GLYPHS[(byte - b'0') as usize]
    } else {
        [0; 7]
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn converts_both_gop_pixel_orders() {
        let rgb = unsafe { BootFramebuffer::new(1, 320, 200, 1280, 32, 0) }.unwrap();
        let bgr = unsafe { BootFramebuffer::new(1, 320, 200, 1280, 32, 1) }.unwrap();
        assert_eq!(rgb.rgb(0x12, 0x34, 0x56), 0x0056_3412);
        assert_eq!(bgr.rgb(0x12, 0x34, 0x56), 0x0012_3456);
    }

    #[test]
    fn draws_panel_text_and_all_progress_segments() {
        let mut pixels = std::vec![0u32; 320 * 200];
        let fb =
            unsafe { BootFramebuffer::new(pixels.as_mut_ptr() as u64, 320, 200, 320 * 4, 32, 1) }
                .unwrap();
        unsafe { fb.draw_stage(KERNEL_STAGE_COUNT, KERNEL_STAGE_COUNT, b"GRAPHICS READY") };
        assert!(pixels.iter().filter(|&&pixel| pixel != 0).count() > 10_000);
        assert!(pixels.contains(&fb.rgb(233, 69, 96)));
        assert!(pixels.contains(&fb.rgb(54, 132, 246)));
        assert!(pixels.contains(&fb.rgb(210, 71, 198)));
    }
}
