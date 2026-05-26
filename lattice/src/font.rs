//! 8×16 fixed‑width bitmap font with PSF2 loader.
//!
//! Two font sources are supported:
//!
//! 1. **Embedded font** — compiled into the kernel via `build.rs`
//!    (95 glyphs, 8×16, 1520 bytes).  Always available.
//!
//! 2. **PSF2 font** — loaded at runtime from a PSF2 file.  Supports
//!    up to 65535 glyphs (including Unicode mapping table).  When
//!    loaded, replaces the embedded font for rendering.
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
pub const GLYPH_HEIGHT: u32 = 16;
pub const GLYPH_BYTES: usize = 16;
pub const GLYPH_COUNT: usize = 95;

/// Raw font bitmap — 95 glyphs × 16 bytes each = 1520 bytes.
/// Compiled at build time by `build.rs`.
static FONT_BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/font8x16.bin"));

// ── PSF2 support ──────────────────────────────────────────────

/// PSF2 magic bytes (little‑endian).
const PSF2_MAGIC: u32 = 0x864AB572;

/// PSF2 header size in bytes.
const PSF2_HEADER_SIZE: u32 = 32;

/// PSF2 flag: Unicode mapping table present.
const PSF2_HAS_UNICODE_TABLE: u32 = 1;

/// Parsed PSF2 header.
#[derive(Debug, Clone, Copy)]
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

    let header_size = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    let flags = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
    let glyph_count = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);
    let glyph_bytes = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
    let height = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
    let width = u32::from_le_bytes([data[28], data[29], data[30], data[31]]);

    let _header = Psf2Header {
        glyph_count,
        glyph_bytes,
        height,
        width,
        has_unicode_table: (flags & PSF2_HAS_UNICODE_TABLE) != 0,
    };

    // We require 8×16 for compatibility with the embedded font.
    if width != 8 || height != 16 {
        return Err("PSF2 font must be 8×16");
    }

    let bitmap_size = (glyph_count as usize).saturating_mul(glyph_bytes as usize);
    let bitmap_start = header_size as usize;
    let bitmap_end = bitmap_start.saturating_add(bitmap_size);

    if data.len() < bitmap_end {
        return Err("PSF2 data truncated (bitmap exceeds data)");
    }

    let bitmap: &'static [u8] =
        unsafe { core::slice::from_raw_parts(data.as_ptr().add(bitmap_start), bitmap_size) };

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
}

/// Look up a glyph for printable ASCII (0x20–0x7E).
///
/// If a PSF2 font has been loaded via [`load_psf2`], glyphs 0x20–0x7E
/// are taken from the PSF bitmap.  Otherwise the compile‑time embedded
/// font is used.
///
/// Characters outside 0x20–0x7E fall back to the space glyph (index 0).
#[inline]
pub fn glyph(ch: u8) -> Glyph<'static> {
    // Try PSF font first
    if let Some(ref psf) = *PSF_FONT.lock() {
        return psf_glyph(psf, ch);
    }
    // Fall back to embedded font
    embedded_glyph(ch)
}

fn embedded_glyph(ch: u8) -> Glyph<'static> {
    let idx = if ch >= 0x20 && ch <= 0x7E {
        (ch - 0x20) as usize
    } else {
        0
    };
    let start = idx * GLYPH_BYTES;
    let end = (start + GLYPH_BYTES).min(FONT_BIN.len());
    Glyph { rows: &FONT_BIN[start..end] }
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

    // SAFETY: The PSF bitmap lives as long as the kernel (`'static`).
    // The caller (glyph) already holds the PSF_FONT lock, so the
    // reference remains valid.
    let rows: &'static [u8] =
        unsafe { core::slice::from_raw_parts(psf.bitmap.as_ptr().add(start), end - start) };

    Glyph { rows }
}

#[inline]
pub fn get_glyph_pixel(ch: u8, row: u32, col: u32) -> bool {
    glyph(ch).pixel(row, col)
}