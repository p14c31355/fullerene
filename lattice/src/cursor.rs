/// Software cursor state.
///
/// The compositor draws the cursor on top of everything.
/// Later this can be replaced with hardware cursor support.
#[derive(Debug, Clone)]
pub struct Cursor {
    pub x: i32,
    pub y: i32,
    /// Whether the cursor should be visible.
    pub visible: bool,
}

impl Cursor {
    pub const SIZE: u32 = 16;
    pub const HOTSPOT_X: i32 = 1;
    pub const HOTSPOT_Y: i32 = 1;

    /// Pre‑rendered arrow bitmap (opaque white triangle, zero = transparent).
    ///
    /// Generated once at compile time via `generate_shape()`.
    pub const SHAPE_PIXELS: [u32; (Self::SIZE as usize) * (Self::SIZE as usize)] = generate_shape();

    pub fn new(x: i32, y: i32) -> Self {
        Self {
            x,
            y,
            visible: true,
        }
    }

    /// Return a reference to the cursor shape data.
    #[inline]
    pub fn shape() -> &'static [u32] {
        &Self::SHAPE_PIXELS
    }
}

/// const fn that builds a 16×16 white arrow bitmap.
///
/// Zero values are transparent; `0xFFFFFFFF` is opaque white.
const fn generate_shape() -> [u32; 256] {
    let mut pixels = [0u32; 256];
    let w = 16usize;
    let arrow_h = 12usize;

    let mut y = 0;
    while y < arrow_h {
        let mut x = 0;
        while x <= y {
            pixels[y * w + x] = 0xFFFFFFFF;
            x += 1;
        }
        y += 1;
    }
    pixels
}
