//! Integer rectangles shared by display and compositor code.
//!
//! A display pipeline needs one rectangle type and one definition of "clip"
//! and "union". Duplicating that arithmetic per backend is how dirty-region
//! bugs start, so it lives here and every platform uses it.

/// An axis-aligned rectangle in surface coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The rectangle covering a whole surface.
    pub const fn surface(width: u16, height: u16) -> Self {
        Self::new(0, 0, width, height)
    }

    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub const fn pixels(&self) -> u32 {
        self.width as u32 * self.height as u32
    }

    /// Bytes the region occupies as RGB565.
    pub const fn rgb565_bytes(&self) -> u32 {
        self.pixels() * 2
    }

    pub const fn right(&self) -> u16 {
        self.x.saturating_add(self.width)
    }

    pub const fn bottom(&self) -> u16 {
        self.y.saturating_add(self.height)
    }

    pub const fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// Limit the rectangle to a `width` x `height` surface.
    ///
    /// Returns `None` when nothing of it is visible, so callers do not have to
    /// special-case off-screen regions.
    pub fn clipped(self, width: u16, height: u16) -> Option<Self> {
        if self.is_empty() || self.x >= width || self.y >= height {
            return None;
        }
        Some(Self {
            x: self.x,
            y: self.y,
            width: self.width.min(width - self.x),
            height: self.height.min(height - self.y),
        })
    }

    /// The smallest rectangle containing both, unclipped.
    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let left = self.x.min(other.x);
        let top = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Self {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        }
    }

    /// Row-major offset of the top-left pixel inside a surface of that width.
    pub fn row_offset(self, surface_width: u16) -> usize {
        usize::from(self.y) * usize::from(surface_width) + usize::from(self.x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping_stops_at_the_surface_edge() {
        let clipped = Rect::new(300, 200, 40, 40).clipped(320, 240).unwrap();
        assert_eq!(clipped, Rect::new(300, 200, 20, 40));
        assert!(Rect::new(320, 0, 1, 1).clipped(320, 240).is_none());
        assert!(Rect::new(0, 0, 0, 10).clipped(320, 240).is_none());
    }

    #[test]
    fn union_covers_both_regions() {
        let a = Rect::new(10, 10, 20, 20);
        let b = Rect::new(40, 5, 10, 10);
        assert_eq!(a.union(b), Rect::new(10, 5, 40, 25));
        assert_eq!(a.union(Rect::new(12, 12, 4, 4)), a);
        // Empty operands are identities rather than collapsing the result.
        assert_eq!(a.union(Rect::default()), a);
        assert_eq!(Rect::default().union(b), b);
    }

    #[test]
    fn a_small_marker_is_a_fraction_of_a_frame() {
        let full = Rect::surface(320, 240);
        let marker = Rect::new(40, 40, 12, 12);
        assert_eq!(full.rgb565_bytes(), 320 * 240 * 2);
        assert_eq!(marker.rgb565_bytes(), 288);
        assert!(marker.rgb565_bytes() * 500 < full.rgb565_bytes());
    }

    #[test]
    fn containment_uses_half_open_intervals() {
        let rect = Rect::new(10, 10, 10, 10);
        assert!(rect.contains(10, 10));
        assert!(rect.contains(19, 19));
        assert!(!rect.contains(20, 19));
        assert!(!rect.contains(9, 10));
    }

    #[test]
    fn row_offset_matches_row_major_layout() {
        let rect = Rect::new(3, 2, 4, 4);
        assert_eq!(rect.row_offset(320), 2 * 320 + 3);
    }
}
