use crate::surface::Surface;

/// Opaque identifier for a window.
///
/// Using an integer ID instead of direct references keeps the WM in control
/// of window lifetime and avoids shared‑ownership complexity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(pub u64);

impl WindowId {
    pub const INVALID: WindowId = WindowId(u64::MAX);
}

/// The simplest possible window.
///
/// Later we will add:
/// - `title`
/// - `focused` flag
/// - resize handles
/// - decorations (title bar, border)
pub struct Window {
    pub id: WindowId,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub surface: Surface,
}

impl Window {
    /// Create a new window with a solid‑color surface.
    pub fn new(id: WindowId, x: i32, y: i32, width: u32, height: u32, color: u32) -> Self {
        Self {
            id,
            x,
            y,
            width,
            height,
            surface: Surface::new(width, height, color),
        }
    }

    /// Check whether a point (in screen coordinates) lies inside this window.
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x
            && py >= self.y
            && px < self.x + self.width as i32
            && py < self.y + self.height as i32
    }
}
