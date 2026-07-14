//! Font rendering — bitmap fallback via embedded-graphics + ab_glyph TrueType.
//!
//! When a TTF font is available (compiled at build time), text is
//! rendered with grayscale antialiasing.  Otherwise a built‑in bitmap
//! font from `embedded-graphics` is used as fallback.
//!
//! # PSF2 Header (32 bytes)
//!
//! ```text
//! Offset  Size  Description
//! 0       4     Magic: 0x864AB572
//! 4       4     Version (0)
//! 8       4     Header size (32)
//! 12      4     Flags (0 = no Unicode table, 1 = has Unicode table)
//! 16      4     Number of glyphs
//! 20      4     Bytes per glyph
//! 24      4     Height (rows)
//! 28      4     Width (cols)
//! ```
//!
//! After the header: glyph bitmap data (glyph_count × glyph_bytes),
//! followed by an optional Unicode mapping table.

pub const GLYPH_WIDTH: u32 = 8;
pub const GLYPH_HEIGHT: u32 = 13;
pub const GLYPH_BYTES: usize = 13;
pub const GLYPH_COUNT: usize = 95;

use embedded_graphics::mono_font::ascii::{FONT_8X13, FONT_9X15, FONT_6X12, FONT_10X20};

use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::Pixel;

/// Simple draw target that renders BinaryColor pixels onto a `&mut [u32]` framebuffer.
struct FbDrawTarget<'a> {
    fb: &'a mut [u32],
    width: u32,
    height: u32,
    stride: u32,
    fg_color: u32,
}

impl DrawTarget for FbDrawTarget<'_> {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(pos, color) in pixels {
            if color == BinaryColor::On {
                let px = pos.x as u32;
                let py = pos.y as u32;
                if px < self.width && py < self.height {
                    self.fb[(py * self.stride + px) as usize] = self.fg_color;
                }
            }
        }
        Ok(())
    }
}

impl OriginDimensions for FbDrawTarget<'_> {
    fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }
}

// ── PSF2 support ──────────────────────────────────────────────

/// PSF2 magic bytes (little‑endian).
const PSF2_MAGIC: u32 = 0x864AB572;

/// PSF2 header size in bytes.
const PSF2_HEADER_SIZE: u32 = 32;

/// Parsed PSF2 header.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct Psf2Header {
    /// Number of glyphs.
    glyph_count: u32,
    /// Bytes per glyph row (must equal GLYPH_HEIGHT for 8×16).
    glyph_bytes: u32,
    /// Height in rows.
    height: u32,
    /// Width in pixels.
    width: u32,
    /// Whether a Unicode mapping table follows the bitmap data.
    has_unicode_table: bool,
}

/// Runtime‑loaded PSF2 font.
///
/// When loaded, all text rendering uses this font instead of the
/// compile‑time embedded font.
static PSF_FONT: spin::Mutex<Option<PsfFont>> = spin::Mutex::new(None);

/// A PSF2 font loaded into memory.
#[allow(dead_code)]
struct PsfFont {
    /// Glyph bitmap data (glyph_count × glyph_bytes).
    bitmap: &'static [u8],
    /// Bytes per glyph.
    glyph_bytes: u32,
    /// Number of glyphs.
    glyph_count: u32,
    /// Height in rows.
    height: u32,
}

/// Try to load a PSF2 font from raw bytes.
///
/// Returns `Ok(())` on success — subsequent calls to [`glyph()`] will
/// use the PSF glyphs.  Returns `Err(&str)` with a human‑readable
/// error if the data is not valid PSF2 or the glyph dimensions don't
/// match 8×16.
///
/// # Safety
///
/// The caller must ensure `data` remains valid for the lifetime of the
/// kernel (it is stored as `&'static [u8]`).
pub fn load_psf2(data: &'static [u8]) -> Result<(), &'static str> {
    if data.len() < PSF2_HEADER_SIZE as usize {
        return Err("PSF2 data too short for header");
    }

    let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if magic != PSF2_MAGIC {
        return Err("not a PSF2 font (bad magic)");
    }

    let _header_size = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    let _flags = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
    let glyph_count = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);
    let glyph_bytes = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
    let height = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
    let width = u32::from_le_bytes([data[28], data[29], data[30], data[31]]);

    // We require 8×16 for compatibility with the embedded font.
    if width != 8 || height != 16 {
        return Err("PSF2 font must be 8×16");
    }

    let header_size = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    let bitmap_size = (glyph_count as usize).saturating_mul(glyph_bytes as usize);
    let bitmap_start = header_size as usize;
    let bitmap_end = bitmap_start.saturating_add(bitmap_size);

    if data.len() < bitmap_end {
        return Err("PSF2 data truncated (bitmap exceeds data)");
    }

    let bitmap: &'static [u8] = &data[bitmap_start..bitmap_end];

    *PSF_FONT.lock() = Some(PsfFont {
        bitmap,
        glyph_bytes,
        glyph_count,
        height,
    });

    Ok(())
}

/// Unload any PSF font and revert to the embedded compile‑time font.
pub fn unload_psf() {
    *PSF_FONT.lock() = None;
}

/// Returns `true` when a PSF2 font has been loaded.
pub fn psf_loaded() -> bool {
    PSF_FONT.lock().is_some()
}

// ── Unicode / fallback glyphs ─────────────────────────────────

/// Pre‑baked fallback glyphs for Unicode codepoints.
///
/// These are 16‑byte rows matching GLYPH_HEIGHT.
mod fallback {
    /// Full‑block replacement character (U+FFFD style).
    pub const REPLACEMENT: [u8; 16] = [
        0x7E, 0x81, 0xA5, 0x81, 0x81, 0xBD, 0x81, 0x81, 0x81, 0x81, 0xBD, 0x81, 0xA5, 0x81, 0x7E,
        0x00,
    ];

    /// Hollow square for bullets / unknown.
    pub const HOLLOW_SQUARE: [u8; 16] = [
        0xFF, 0x81, 0x81, 0x81, 0x81, 0x81, 0x81, 0x81, 0x81, 0x81, 0x81, 0x81, 0x81, 0x81, 0x81,
        0xFF,
    ];

    /// Middle dot (for interpunct / separator).
    pub const MIDDLE_DOT: [u8; 16] = [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x00, 0x00, 0x18, 0x18, 0x00, 0x00, 0x00, 0x00,
        0x00,
    ];

    /// Simple bullet.
    pub const BULLET: [u8; 16] = [
        0x00, 0x00, 0x00, 0x00, 0x18, 0x3C, 0x3C, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00,
    ];
}

/// Return the best glyph for a character code, using a fallback chain:
/// 1. PSF2 font (if loaded)
/// 2. Embedded ASCII font (0x20–0x7E)
/// 3. Unicode fallback (full‑block, hollow square, '?' etc.)
pub fn glyph_for_codepoint(cp: u32) -> Glyph<'static> {
    // Fast path: ASCII
    if cp <= 0x7E {
        let ch = cp as u8;
        if ch >= 0x20 {
            return ascii_glyph(ch);
        }
        // Control characters: space
        return ascii_glyph(b' ');
    }

    // PSF2 Unicode table lookups for codepoints > 0x7E go here.
    // Currently not implemented; unicode fallback handles non‑ASCII.
    let _ = PSF_FONT.lock();

    // Unicode fallback chain
    match cp {
        0x2022 => Glyph {
            rows: &fallback::BULLET,
        }, // •
        0x2026 => Glyph {
            rows: &fallback::MIDDLE_DOT,
        }, // …
        0x25A0 | 0x25A1 | 0x25A2 => Glyph {
            rows: &fallback::HOLLOW_SQUARE,
        }, // ■ □ ▢
        0x25CF => Glyph {
            rows: &fallback::BULLET,
        }, // ●
        0xFFFD | 0xFFFE => Glyph {
            rows: &fallback::REPLACEMENT,
        }, // replacement char
        _ => Glyph {
            rows: &fallback::HOLLOW_SQUARE,
        }, // unknown → □
    }
}

// ── Glyph access ──────────────────────────────────────────────

pub struct Glyph<'a> {
    rows: &'a [u8],
}

impl Glyph<'_> {
    #[inline]
    pub fn pixel(&self, row: u32, col: u32) -> bool {
        let idx = row as usize;
        if idx >= self.rows.len() {
            return false;
        }
        let byte = self.rows[idx];
        byte & (0x80 >> col) != 0
    }

    /// Return the raw bitmap byte for a given row (0..height).
    /// Returns 0 if `row` is out of bounds.  Callers can test
    /// individual bits with `byte & (0x80 >> col) != 0`.
    #[inline]
    pub fn row_byte(&self, row: u32) -> u8 {
        let idx = row as usize;
        if idx < self.rows.len() {
            self.rows[idx]
        } else {
            0
        }
    }
}

/// Look up a glyph for printable ASCII (0x20–0x7E).
///
/// If a PSF2 font has been loaded via [`load_psf2`], glyphs 0x20–0x7E
/// are taken from the PSF bitmap.  Otherwise the font from
/// `embedded-graphics` is used.
///
/// Characters outside 0x20–0x7E fall back to the space glyph (index 0).
/// Use [`glyph_for_codepoint`] for Unicode‑aware glyph rendering.
#[inline]
pub fn glyph(ch: u8) -> Glyph<'static> {
    if ch >= 0x20 && ch <= 0x7E {
        ascii_glyph(ch)
    } else {
        ascii_glyph(b' ')
    }
}

fn ascii_glyph(ch: u8) -> Glyph<'static> {
    if let Some(ref psf) = *PSF_FONT.lock() {
        return psf_glyph(psf, ch);
    }
    embedded_glyph(ch)
}

/// Render a single ASCII character into a tiny 8×13 framebuffer and return
/// the bitmap rows as a glyph.  The result is cached in [`EMBEDDED_GLYPH_CACHE`]
/// so the rasterisation only happens once.
fn embedded_glyph(ch: u8) -> Glyph<'static> {
    let idx = if ch >= 0x20 && ch <= 0x7E {
        (ch - 0x20) as usize
    } else {
        0
    };
    let rows = embedded_glyph_data();
    let start = idx * GLYPH_BYTES;
    let end = (start + GLYPH_BYTES).min(rows.len());
    Glyph { rows: &rows[start..end] }
}

/// Lazily rasterise all 95 ASCII glyphs into a flat byte array via
/// `embedded-graphics` and cache the result.
fn embedded_glyph_data() -> &'static [u8] {
    use embedded_graphics::mono_font::MonoTextStyle;
    use embedded_graphics::text::{Text, Baseline};
    static CACHE: spin::once::Once<[u8; 1235]> = spin::once::Once::new();
    CACHE.call_once(|| {
        let mut out = [0u8; 1235];
        let mut buf = [0u32; 8 * 13];
        for code in 0x20u8..=0x7Eu8 {
            let i = (code - 0x20) as usize;
            buf.fill(0);
            let mut target = FbDrawTarget {
                fb: &mut buf,
                width: 8,
                height: 13,
                stride: 8,
                fg_color: 0xFFFFFF,
            };
            let style = MonoTextStyle::new(&FONT_8X13, BinaryColor::On);
            let ch_slice = [code];
            let text_str = core::str::from_utf8(&ch_slice).unwrap_or(" ");
            let _ = Text::with_baseline(text_str, Point::new(0, 0), style, Baseline::Top)
                .draw(&mut target);
            for r in 0..13 {
                let mut byte = 0u8;
                for c in 0..8 {
                    if buf[r * 8 + c] == 0xFFFFFF {
                        byte |= 0x80 >> c;
                    }
                }
                out[i * 13 + r] = byte;
            }
        }
        out
    })
}

fn psf_glyph(psf: &PsfFont, ch: u8) -> Glyph<'static> {
    let idx = if ch >= 0x20 && ch <= 0x7E {
        (ch - 0x20) as usize
    } else {
        0
    };
    let gb = psf.glyph_bytes as usize;
    let start = idx * gb;
    let end = (start + gb).min(psf.bitmap.len());

    let rows: &'static [u8] = &psf.bitmap[start..end];

    Glyph { rows }
}

/// Single glyph pixel lookup with per-call font selection.
///
/// For repeated pixel queries on the same character (e.g. rendering a glyph
/// row-by-row), prefer [`glyph_fast`] to lock the font mutex once instead
/// of on every pixel access.
#[inline]
pub fn get_glyph_pixel(ch: u8, row: u32, col: u32) -> bool {
    glyph(ch).pixel(row, col)
}

/// Return a `Glyph` for ASCII without Mutex contention per pixel.
///
/// This checks PSF once and returns either the PSF glyph or the
/// embedded-graphics glyph, so callers can do many `.pixel()` calls
/// without re‑locking.
#[inline]
pub fn glyph_fast(ch: u8) -> Glyph<'static> {
    if let Some(ref psf) = *PSF_FONT.lock() {
        return psf_glyph(psf, ch);
    }
    embedded_glyph(ch)
}

/// Render a string of bitmap glyphs onto a framebuffer using
/// `embedded-graphics` mono fonts.
///
/// `glyph_height` is typically 12 (standard) or 14 (label).  Each glyph
/// is 8 columns wide.  Characters outside the printable ASCII range
/// (32..=126) are silently skipped.
pub fn render_text(
    fb: &mut [u32], fb_width: u32, fb_height: u32, fb_stride: u32,
    x: u32, y: u32, text: &[u8], color: u32, glyph_height: u32,
) {
    // Use PSF2 font if loaded, otherwise embedded-graphics mono font.
    if psf_loaded() {
        for (i, &ch) in text.iter().enumerate() {
            if ch < 32 || ch > 126 { continue; }
            let gl = glyph_fast(ch);
            let gx = x + (i as u32) * 8;
            for row in 0..glyph_height {
                let py = y + row;
                if py >= fb_height { continue; }
                for col in 0..8 {
                    let px = gx + col;
                    if px >= fb_width { continue; }
                    if gl.pixel(row, col) {
                        fb[(py * fb_stride + px) as usize] = color;
                    }
                }
            }
        }
        return;
    }

    // Default: use embedded-graphics mono font via FbDrawTarget.
    // Select font based on requested glyph_height.
    let font = match glyph_height {
        0..=12 => &FONT_6X12,
        13..=14 => &FONT_8X13,
        15..=19 => &FONT_9X15,
        _ => &FONT_10X20,
    };
    let mut target = FbDrawTarget {
        fb,
        width: fb_width,
        height: fb_height,
        stride: fb_stride,
        fg_color: color,
    };
    use embedded_graphics::text::Baseline as EgBaseline;
    let style = embedded_graphics::mono_font::MonoTextStyle::new(font, BinaryColor::On);
    let text_str = core::str::from_utf8(text).unwrap_or("");
    let _ = embedded_graphics::text::Text::with_baseline(
        text_str, Point::new(x as i32, y as i32), style, EgBaseline::Top,
    )
    .draw(&mut target);
}

/// Convenience wrapper: bitmap text at (x, y) with 12px glyph height.
pub fn render_text_bitmap(
    fb: &mut [u32], fb_width: u32, fb_height: u32, fb_stride: u32,
    x: i32, y: i32, text: &str, color: u32,
) {
    let h = PSF_FONT.lock().as_ref().map_or(GLYPH_HEIGHT, |psf| psf.height);
    if y < 0 || y as u32 + h >= fb_height { return; }
    render_text(fb, fb_width, fb_height, fb_stride, x.max(0) as u32, y as u32, text.as_bytes(), color, h);
}

// ── TrueType font support (ab_glyph) ──────────────────────────

use ab_glyph::{FontArc, PxScale, point};
use ab_glyph::Font as _;
use ab_glyph::ScaleFont as _;
use spin::once::Once;

static TTF_DATA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/LiberationSans-Regular.ttf"));

static TTF_FONT: Once<Option<FontArc>> = Once::new();

/// Try to get the loaded TTF font, or `None` if unavailable.
pub fn get_ttf_font() -> Option<&'static FontArc> {
    TTF_FONT.call_once(|| FontArc::try_from_slice(TTF_DATA).ok());
    TTF_FONT.get().unwrap().as_ref()
}

/// Render text using the TTF font with grayscale antialiasing.
pub fn render_text_ttf(
    fb: &mut [u32], fb_width: u32, fb_height: u32, fb_stride: u32,
    x: i32, y: i32, text: &str, color: u32, size: f32,
    font: &FontArc,
) -> Result<(), ()> {
    let scale = PxScale { x: size, y: size };
    let sf = font.as_scaled(scale);
    let mut px = x as f32;
    let base_y = y as f32 + size * 0.85;
    for ch in text.chars() {
        if ch == ' ' { px += size * 0.35; continue; }
        let gid = sf.glyph_id(ch);
        let glyph = gid.with_scale_and_position(scale, point(px, base_y));
        if let Some(outline) = sf.outline_glyph(glyph) {
            let bounds = outline.px_bounds();
            let ox = bounds.min.x as i32;
            let oy = bounds.min.y as i32;
            outline.draw(|dx, dy, coverage| {
                let bx = ox + dx as i32;
                let by = oy + dy as i32;
                if bx < 0 || by < 0 || bx as u32 >= fb_width || by as u32 >= fb_height { return; }
                let ca = (coverage * 255.0) as u32;
                if ca == 0 { return; }
                let idx = (by as usize) * (fb_stride as usize) + (bx as usize);
                if ca >= 255 { fb[idx] = color; return; }
                let bg = fb[idx];
                let ia = 255 - ca;
                let r = (((color >> 16) & 0xFF) * ca + ((bg >> 16) & 0xFF) * ia) / 255;
                let g = (((color >> 8) & 0xFF) * ca + ((bg >> 8) & 0xFF) * ia) / 255;
                let b = ((color & 0xFF) * ca + (bg & 0xFF) * ia) / 255;
                fb[idx] = (bg & 0xFF00_0000) | (r << 16) | (g << 8) | b;
            });
            px += sf.h_advance(gid);
        } else {
            px += size * 0.5;
        }
    }
    Ok(())
}
