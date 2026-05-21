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

    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y, visible: true }
    }

    /// Generate the cursor pixel data as a small arrow shape.
    ///
    /// Returns `(pixels, width, height)`.
    /// The arrow points up-right, with the hotspot at (0, 0).
    ///
    /// All non‑zero pixels are solid white.
    /// Zero (0x00000000) = transparent (not drawn).
    pub fn shape() -> (Vec<u32>, u32, u32) {
        let w = Self::SIZE as usize;
        let h = Self::SIZE as usize;
        let mut pixels = vec![0u32; w * h];

        // Arrow pointing up-right: filled triangle where each row y has
        // (y+1) columns filled, truncated to the triangle of height 12.
        let arrow_h = 12usize;
        for y in 0..arrow_h {
            for x in 0..=y {
                if x < w && y < h {
                    pixels[y * w + x] = 0xFFFFFF;
                }
            }
        }

        (pixels, Self::SIZE, Self::SIZE)
    }
}