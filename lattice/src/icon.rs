//! Pre-rendered SVG icons — RGBA pixel data generated at build time.
//!
//! Each icon is a 64×64 RGBA pixel buffer embedded into the binary.
//! The Painter or compositor renders them via direct blit.

use crate::surface::Surface;

const ICON_SIZE: u32 = 64;

/// A pre-rendered 64×64 RGBA icon.
pub struct SvgIcon {
    pixels: &'static [u8], // 64*64*4 = 16384 bytes
}

impl SvgIcon {
    const fn from_rgba(data: &'static [u8]) -> Self {
        Self { pixels: data }
    }

    /// Convert the icon to a surface (heap-allocates Vec<u32>).
    pub fn surface(&self) -> Surface {
        let mut s = Surface::new(ICON_SIZE, ICON_SIZE, 0);
        let buf = s.pixels_mut();
        for (i, chunk) in self.pixels.chunks_exact(4).enumerate() {
            buf[i] = rgba_premul_to_u32(chunk);
        }
        s
    }

    /// Blit the icon directly into a framebuffer slice at (x, y).
    /// No heap allocation — pixels are written directly into `fb`.
    pub fn blit_into(&self, fb: &mut [u32], fbw: u32, stride: usize, x: i32, y: i32) {
        let fbw = fbw as i32;
        let fbh = (fb.len() / stride) as i32;
        for row in 0..ICON_SIZE as i32 {
            let dy = y + row;
            if dy < 0 || dy >= fbh {
                continue;
            }
            let row_base = (dy as usize) * stride;
            for col in 0..ICON_SIZE as i32 {
                let dx = x + col;
                if dx < 0 || dx >= fbw {
                    continue;
                }
                let src_idx = (row * ICON_SIZE as i32 + col) as usize * 4;
                let chunk = &self.pixels[src_idx..src_idx + 4];
                let pixel = rgba_premul_to_u32(chunk);
                let idx = row_base + dx as usize;
                if idx < fb.len() {
                    let a = (pixel >> 24) & 0xFF;
                    if a == 255 {
                        fb[idx] = pixel;
                    } else if a > 0 {
                        // Alpha blend over existing background
                        let bg = fb[idx];
                        let ia = 255 - a;
                        let r = (((pixel >> 16) & 0xFF) * a + ((bg >> 16) & 0xFF) * ia) / 255;
                        let g = (((pixel >> 8) & 0xFF) * a + ((bg >> 8) & 0xFF) * ia) / 255;
                        let b = ((pixel & 0xFF) * a + (bg & 0xFF) * ia) / 255;
                        fb[idx] = (bg & 0xFF00_0000) | (r << 16) | (g << 8) | b;
                    }
                }
            }
        }
    }

    /// Blit a nearest-neighbour scaled icon without allocating a temporary
    /// surface. Dock icons are intentionally smaller than desktop icons.
    pub fn blit_scaled_into(
        &self,
        fb: &mut [u32],
        fbw: u32,
        stride: usize,
        x: i32,
        y: i32,
        size: u32,
    ) {
        if size == 0 || stride == 0 {
            return;
        }
        let width = fbw as i32;
        let height = (fb.len() / stride) as i32;
        for row in 0..size as i32 {
            let dy = y + row;
            if dy < 0 || dy >= height {
                continue;
            }
            let source_row = (row as u32 * ICON_SIZE / size) as usize;
            for col in 0..size as i32 {
                let dx = x + col;
                if dx < 0 || dx >= width {
                    continue;
                }
                let source_col = (col as u32 * ICON_SIZE / size) as usize;
                let source = &self.pixels[(source_row * ICON_SIZE as usize + source_col) * 4..];
                let pixel = rgba_premul_to_u32(&source[..4]);
                if pixel == 0 {
                    continue;
                }
                let idx = dy as usize * stride + dx as usize;
                if idx >= fb.len() {
                    continue;
                }
                let alpha = pixel >> 24;
                if alpha == 255 {
                    fb[idx] = pixel;
                } else {
                    let bg = fb[idx];
                    let inverse = 255 - alpha;
                    let r = (((pixel >> 16) & 0xFF) * alpha + ((bg >> 16) & 0xFF) * inverse) / 255;
                    let g = (((pixel >> 8) & 0xFF) * alpha + ((bg >> 8) & 0xFF) * inverse) / 255;
                    let b = ((pixel & 0xFF) * alpha + (bg & 0xFF) * inverse) / 255;
                    fb[idx] = (bg & 0xFF00_0000) | (r << 16) | (g << 8) | b;
                }
            }
        }
    }
}

/// Convert 4 premultiplied RGBA bytes to a u32 in 0xAARRGGBB format.
#[inline]
fn rgba_premul_to_u32(chunk: &[u8]) -> u32 {
    let r_pre = chunk[0] as u32;
    let g_pre = chunk[1] as u32;
    let b_pre = chunk[2] as u32;
    let a = chunk[3] as u32;
    if a == 0 {
        return 0;
    }
    let r = (r_pre * 255) / a;
    let g = (g_pre * 255) / a;
    let b = (b_pre * 255) / a;
    (r << 16) | (g << 8) | b | (a << 24)
}

pub static ICON_SHELL: SvgIcon =
    SvgIcon::from_rgba(include_bytes!(concat!(env!("OUT_DIR"), "/icon_shell.rgba")));
pub static ICON_TERMINAL: SvgIcon = SvgIcon::from_rgba(include_bytes!(concat!(
    env!("OUT_DIR"),
    "/icon_terminal.rgba"
)));
pub static ICON_EDITOR: SvgIcon = SvgIcon::from_rgba(include_bytes!(concat!(
    env!("OUT_DIR"),
    "/icon_editor.rgba"
)));
pub static ICON_CLOCK: SvgIcon =
    SvgIcon::from_rgba(include_bytes!(concat!(env!("OUT_DIR"), "/icon_clock.rgba")));
pub static ICON_FILES: SvgIcon =
    SvgIcon::from_rgba(include_bytes!(concat!(env!("OUT_DIR"), "/icon_files.rgba")));
pub static ICON_SETTINGS: SvgIcon = SvgIcon::from_rgba(include_bytes!(concat!(
    env!("OUT_DIR"),
    "/icon_settings.rgba"
)));
pub static ICON_ABOUT: SvgIcon =
    SvgIcon::from_rgba(include_bytes!(concat!(env!("OUT_DIR"), "/icon_about.rgba")));
