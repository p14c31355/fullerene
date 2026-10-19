//! Scan-out rotation helpers.
//!
//! Some panels are mounted so that the framebuffer that reaches them is
//! taller than it is wide, and a phone-first shell wants to lay out in
//! landscape. The desktop then renders into a landscape canvas and the
//! scan-out rotates it into the portrait panel.
//!
//! The mapping is the part that bites: it is the only place in the flush
//! where two different widths and two different heights are in play at
//! once. v331m wrote a bare `HEIGHT - 1 - px` there and read ~22 MB past
//! a 10 MB canvas - every image from that commit on failed to enumerate.
//! Keeping the map here, with the dimensions as explicit arguments and
//! corner tests, makes that class of error a test failure instead of a
//! dead phone.

/// Index of the source word that the 90-degree-clockwise rotation places
/// at destination `(dx, dy)`.
///
/// `src_w` is the source width in pixels (the landscape long side) and
/// `src_h` the source height (the short side). A source point `(sx, sy)`
/// lands at destination `(dx, dy) = (src_h - 1 - sy, sx)`, so the inverse
/// is `sx = dy`, `sy = src_h - 1 - dx`.
///
/// # Panics
///
/// Debug builds assert that the destination point lies inside the rotated
/// image: `dx < src_h`, `dy < src_w`.
pub fn cw90_index(dx: u32, dy: u32, src_w: u32, src_h: u32) -> usize {
    debug_assert!(dx < src_h, "dst column {dx} outside rotated width {src_h}");
    debug_assert!(dy < src_w, "dst row {dy} outside rotated height {src_w}");
    let sy = src_h.wrapping_sub(1).wrapping_sub(dx);
    sy as usize * src_w as usize + dy as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    const LW: u32 = 2340; // landscape long side
    const LH: u32 = 1080; // landscape short side

    #[test]
    fn stays_inside_the_source_image() {
        let last = (LW as usize) * (LH as usize) - 1;
        for &(dx, dy) in &[(0, 0), (LH - 1, 0), (0, LW - 1), (LH - 1, LW - 1)] {
            assert!(cw90_index(dx, dy, LW, LH) <= last);
        }
    }

    #[test]
    fn every_destination_pixel_is_covered_exactly_once() {
        // A small image keeps the bijection check alloc-free.
        const W: u32 = 6;
        const H: u32 = 4;
        let mut seen = [false; (W * H) as usize];
        for dy in 0..W {
            for dx in 0..H {
                let i = cw90_index(dx, dy, W, H);
                assert!(!seen[i], "index {i} written twice");
                seen[i] = true;
            }
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn corners_rotate_clockwise() {
        // Landscape top-left goes to the rotated top-right.
        assert_eq!(cw90_index(LH - 1, 0, LW, LH), 0);
        // Landscape top-right goes to the rotated bottom-right.
        assert_eq!(cw90_index(LH - 1, LW - 1, LW, LH), (LW - 1) as usize);
        // Landscape bottom-left goes to the rotated top-left.
        assert_eq!(cw90_index(0, 0, LW, LH), (LH - 1) as usize * LW as usize);
        // Landscape bottom-right ends at the last word of the image.
        assert_eq!(
            cw90_index(0, LW - 1, LW, LH),
            (LH as usize) * (LW as usize) - 1
        );
    }

    #[test]
    fn a_row_of_the_source_becomes_a_column_of_the_destination() {
        let sy = 0; // the landscape image's first row
        let dx = LH - 1 - sy;
        for dy in [0u32, 1, LW / 2, LW - 1] {
            assert_eq!(cw90_index(dx, dy, LW, LH), dy as usize);
        }
    }
}
