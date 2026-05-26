use crate::cursor::Cursor;
use crate::window::Window;

/// A rectangular dirty/update region in pixel coordinates.
///
/// Used to track which areas of the framebuffer need redrawing.
/// Multiple dirty rects may overlap — the compositor clips to the
/// framebuffer and draws only the union of all dirty regions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirtyRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl DirtyRect {
    /// Create a new dirty rect.
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }

    /// Return the full framebuffer area as a single dirty rect.
    pub const fn full(fb_width: u32, fb_height: u32) -> Self {
        Self { x: 0, y: 0, width: fb_width, height: fb_height }
    }

    /// Check if this rect intersects with another.
    pub fn intersects(&self, other: &DirtyRect) -> bool {
        self.x < other.x + other.width
            && self.x + self.width > other.x
            && self.y < other.y + other.height
            && self.y + self.height > other.y
    }

    /// Merge another dirty rect into this one (union).
    pub fn merge(&mut self, other: &DirtyRect) {
        let x1 = self.x.min(other.x);
        let y1 = self.y.min(other.y);
        let x2 = (self.x + self.width).max(other.x + other.width);
        let y2 = (self.y + self.height).max(other.y + other.height);
        self.x = x1;
        self.y = y1;
        self.width = x2 - x1;
        self.height = y2 - y1;
    }
}

/// An immutable snapshot of the desktop state for the compositor.
///
/// `Scene` is pure data — it carries references to whatever the compositor
/// needs to draw, without owning or mutating anything.
///
/// The `dirty_rects` field controls partial redraw: when empty, the
/// compositor redraws the full framebuffer (legacy behaviour).  When
/// populated, only pixels covered by those rects are updated.
///
/// ```ignore
/// let scene = desktop.scene();
/// compositor.render(&scene, &mut target);
/// ```
#[derive(Clone)]
pub struct Scene<'a> {
    pub windows: &'a [Window],
    pub cursor: Option<&'a Cursor>,
    pub bg_color: u32,
    /// Dirty / update regions for partial redraw.
    /// Empty → force full redraw (legacy / fallback).
    pub dirty_rects: &'a [DirtyRect],
}

impl<'a> Scene<'a> {
    pub fn new(
        windows: &'a [Window],
        cursor: Option<&'a Cursor>,
        bg_color: u32,
    ) -> Self {
        Self {
            windows,
            cursor,
            bg_color,
            dirty_rects: &[],
        }
    }

    pub fn with_dirty_rects(
        windows: &'a [Window],
        cursor: Option<&'a Cursor>,
        bg_color: u32,
        dirty_rects: &'a [DirtyRect],
    ) -> Self {
        Self {
            windows,
            cursor,
            bg_color,
            dirty_rects,
        }
    }
}
