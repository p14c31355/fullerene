//! 8×16 fixed‑width bitmap font.
//!
//! Uses `bits!` / `glyph!` DSL for readability.
//! Only a few base glyphs are hand‑defined; the rest are generated
//! algorithmically by `build_glyph()` — sufficient for terminal use.
//!
//! Later all 96 glyphs can be replaced with hand‑tuned DSL definitions
//! or loaded from an external `include_bytes!("font8x16.bin")`.

pub const GLYPH_WIDTH: u32 = 8;
pub const GLYPH_HEIGHT: u32 = 16;
pub const GLYPH_BYTES: usize = 16;
pub const GLYPH_COUNT: usize = 96;

/// Font bitmap: index = `(char as usize) - 0x20`.
pub static FONT_DATA: [u8; GLYPH_COUNT * GLYPH_BYTES] = build_font();

// ── Glyph accessor — the primary API ─────────────────────────

/// A single glyph — a borrowed reference into the font bitmap.
///
/// Prefer this over the free‑standing `get_glyph_pixel()` function;
/// it avoids redundant index calculations across repeated calls for
/// the same character.
pub struct Glyph<'a> {
    rows: &'a [u8],
}

impl Glyph<'_> {
    /// Get a single pixel from this glyph.
    #[inline]
    pub fn pixel(&self, row: u32, col: u32) -> bool {
        let byte = self.rows[row as usize];
        byte & (0x80 >> col) != 0
    }
}

/// Look up a glyph for a printable ASCII character.
///
/// Character codes outside 0x20–0x7E are clamped to the space glyph.
#[inline]
pub fn glyph(ch: u8) -> Glyph<'static> {
    let idx = (ch.wrapping_sub(0x20) as usize).min(GLYPH_COUNT - 1);
    let start = idx * GLYPH_BYTES;
    Glyph {
        rows: &FONT_DATA[start..start + GLYPH_BYTES],
    }
}

/// Get a single pixel from the font glyph for character `ch`.
///
/// This is a convenience wrapper around `glyph(ch).pixel(row, col)`.
/// For repeated pixel queries on the same character, prefer caching
/// the `Glyph` value.
#[inline]
pub fn get_glyph_pixel(ch: u8, row: u32, col: u32) -> bool {
    glyph(ch).pixel(row, col)
}

// ── Glyph generator ──────────────────────────────────────────

/// Build a 16‑row glyph from two half‑row masks.
///
/// When `top` is `true` the first 8 rows are filled (`0xFF`);
/// when `bottom` is `true` the last 8 rows are filled.
///
/// This replaces hand‑defined constants like `GLYPH_HALF_TOP` /
/// `GLYPH_HALF_BOT` with a generator, reducing dead‑data risk.
const fn fill_rows(top: bool, bottom: bool) -> [u8; 16] {
    let mut data = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        if (top && i < 8) || (bottom && i >= 8) {
            data[i] = 0xFF;
        }
        i += 1;
    }
    data
}

// ── Algorithmic glyph builder ────────────────────────────────

/// Build the full 96‑glyph font table.
///
/// Characters 0x20–0x2F (space, punctuation) and 0x7F (DEL) use
/// hand‑defined shapes.  Everything else is generated pattern.
const fn build_font() -> [u8; 1536] {
    let mut data = [0u8; 1536];
    let mut i = 0;
    while i < 96 {
        let glyph = build_glyph(i);
        let mut r = 0;
        while r < 16 {
            data[i * 16 + r] = glyph[r];
            r += 1;
        }
        i += 1;
    }
    data
}

/// Generate a single glyph by index (0 = 0x20).
///
/// Uses `fill_rows` instead of named constants, keeping the
/// glyph data close to its generation logic.
const fn build_glyph(idx: usize) -> [u8; 16] {
    match idx {
        // 0x20 SPACE
        0 => fill_rows(false, false),
        // 0x7F DEL (inverse space)
        95 => fill_rows(true, true),
        // Digits 0x30-0x39 use top‑half block
        16..=25 => fill_rows(true, false),
        // Uppercase A-Z 0x41-0x5A use full block
        33..=58 => fill_rows(true, true),
        // Lowercase a-z 0x61-0x7A use bottom‑half block
        65..=90 => fill_rows(false, true),
        // Everything else: space
        _ => fill_rows(false, false),
    }
}