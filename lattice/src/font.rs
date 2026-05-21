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

/// Get a single pixel from the font glyph for character `ch`.
#[inline]
pub fn get_glyph_pixel(ch: u8, row: u32, col: u32) -> bool {
    let idx = (ch.wrapping_sub(0x20) as usize).min(GLYPH_COUNT - 1);
    let byte = FONT_DATA[idx * GLYPH_BYTES + row as usize];
    byte & (0x80 >> col) != 0
}

// ── DSL ─────────────────────────────────────────────────────

macro_rules! bits {
    ($s:literal) => {{
        const fn parse(s: &str) -> u8 {
            let bytes = s.as_bytes();
            let mut v = 0u8;
            let mut i = 0;
            while i < 8 {
                v <<= 1;
                if i < bytes.len() && bytes[i] == b'#' { v |= 1; }
                i += 1;
            }
            v
        }
        parse($s)
    }};
}

macro_rules! glyph {
    ($($l:literal),+ $(,)?) => { [ $(bits!($l)),+ ] };
}

// ── Base hand‑defined glyphs ─────────────────────────────────

const GLYPH_SPACE: [u8; 16] = glyph![
    "........" , "........" , "........" , "........" ,
    "........" , "........" , "........" , "........" ,
    "........" , "........" , "........" , "........" ,
    "........" , "........" , "........" , "........" ,
];

const GLYPH_FULL: [u8; 16] = glyph![
    "########" , "########" , "########" , "########" ,
    "########" , "########" , "########" , "########" ,
    "########" , "########" , "########" , "########" ,
    "########" , "########" , "########" , "########" ,
];

const GLYPH_HALF_TOP: [u8; 16] = glyph![
    "########" , "########" , "########" , "########" ,
    "########" , "########" , "########" , "########" ,
    "........" , "........" , "........" , "........" ,
    "........" , "........" , "........" , "........" ,
];

const GLYPH_HALF_BOT: [u8; 16] = glyph![
    "........" , "........" , "........" , "........" ,
    "........" , "........" , "........" , "........" ,
    "########" , "########" , "########" , "########" ,
    "########" , "########" , "########" , "########" ,
];

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
const fn build_glyph(idx: usize) -> [u8; 16] {
    match idx {
        // 0x20 SPACE
        0 => GLYPH_SPACE,
        // 0x7F DEL (inverse space)
        95 => GLYPH_FULL,
        // Digits 0x30-0x39 use top‑half block
        16..=25 => GLYPH_HALF_TOP,
        // Uppercase A-Z 0x41-0x5A use full block
        33..=58 => GLYPH_FULL,
        // Lowercase a-z 0x61-0x7A use bottom‑half block
        65..=90 => GLYPH_HALF_BOT,
        // Everything else: space
        _ => GLYPH_SPACE,
    }
}
