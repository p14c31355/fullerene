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
    /// The arrow points up-left, with the hotspot at (1, 1).
    pub fn shape() -> (Vec<u32>, u32, u32) {
        let w = Self::SIZE as usize;
        let h = Self::SIZE as usize;
        let mut pixels = vec![0u32; w * h];

        for y in 0..h {
            for x in 0..w {
                // Arrow pattern: upper‑left triangle
                if x <= y && x < h - y {
                    // Outline (black)
                    if x == y || x == 0 || y == 0 || y == Self::SIZE as usize - 1 {
                        pixels[y * w + x] = 0x000000;
                    } else {
                        pixels[y * w + x] = 0xFFFFFF;
                    }
                }
            }
        }

        (pixels, Self::SIZE, Self::SIZE)
    }
}