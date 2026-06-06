use crate::surface::Surface;
use alloc::string::String;

/// Opaque identifier for a window.
///
/// Using an integer ID instead of direct references keeps the WM in control
/// of window lifetime and avoids shared‑ownership complexity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(pub u64);

impl WindowId {
    pub const INVALID: WindowId = WindowId(u64::MAX);
}

/// A window with decorations support.
///
/// Fields:
/// - `title` — optional title bar text (None → no title bar drawn)
/// - `focused` — whether this window has keyboard/mouse focus
///   (affects title bar / border colour)
/// - `minimized` — window is hidden (not drawn)
/// - `maximized` — window fills the work area
/// - `restore_rect` — saved geometry before maximize
/// - `shadow_surface` — optional pre‑rendered shadow surface for drop shadows
pub struct Window {
    pub id: WindowId,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub surface: Surface,
    /// Title text displayed in the title bar.  `None` suppresses the
    /// title bar entirely (no decorations).
    pub title: Option<String>,
    /// Whether this window currently has input focus.
    pub focused: bool,
    /// Whether the window is minimized (hidden).
    pub minimized: bool,
    /// Whether the window is maximized (fills work area).
    pub maximized: bool,
    /// Saved geometry before maximize, so it can be restored.
    pub restore_rect: Option<(i32, i32, u32, u32)>,
    /// Optional drop‑shadow surface drawn behind the window.
    pub shadow_surface: Option<Surface>,
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
            title: None,
            focused: false,
            minimized: false,
            maximized: false,
            restore_rect: None,
            shadow_surface: None,
        }
    }

    /// Create a window with a title bar.
    pub fn new_with_title(
        id: WindowId,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        color: u32,
        title: impl Into<String>,
    ) -> Self {
        Self {
            id,
            x,
            y,
            width,
            height,
            surface: Surface::new(width, height, color),
            title: Some(title.into()),
            focused: false,
            minimized: false,
            maximized: false,
            restore_rect: None,
            shadow_surface: None,
        }
    }

    /// Check whether a point (in screen coordinates) lies inside
    /// this window's **client area** (excluding title bar if present).
    pub fn contains(&self, px: i32, py: i32) -> bool {
        let title_h = if self.title.is_some() {
            crate::compositor::TITLE_BAR_HEIGHT as i32
        } else {
            0i32
        };
        px >= self.x
            && py >= self.y + title_h
            && px < self.x + self.width as i32
            && py < self.y + title_h + self.height as i32
    }

    /// Check whether a point hits the **title bar** area (if present).
    pub fn contains_title_bar(&self, px: i32, py: i32) -> bool {
        if self.title.is_none() {
            return false;
        }
        let title_h = crate::compositor::TITLE_BAR_HEIGHT as i32;
        px >= self.x && py >= self.y && px < self.x + self.width as i32 && py < self.y + title_h
    }

    /// Total decorated width (client area + borders).
    pub fn decorated_width(&self) -> u32 {
        if self.title.is_some() {
            self.width + crate::compositor::WINDOW_BORDER * 2
        } else {
            self.width
        }
    }

    /// Total decorated height (client area + title bar + borders).
    pub fn decorated_height(&self) -> u32 {
        if self.title.is_some() {
            self.height + crate::compositor::TITLE_BAR_HEIGHT + crate::compositor::WINDOW_BORDER * 2
        } else {
            self.height
        }
    }

    /// Hit-test the close button (top-right corner of title bar).
    pub fn hit_close_button(&self, px: i32, py: i32) -> bool {
        if self.title.is_none() || self.minimized {
            return false;
        }
        let bx = self.x + self.width as i32 - 18;
        let by = self.y + 3;
        px >= bx && px < bx + 14 && py >= by && py < by + 14
    }

    /// Hit-test the minimize button (leftmost title bar button).
    pub fn hit_minimize_button(&self, px: i32, py: i32) -> bool {
        if self.title.is_none() || self.minimized {
            return false;
        }
        let bx = self.x + self.width as i32 - 58;
        let by = self.y + 3;
        px >= bx && px < bx + 14 && py >= by && py < by + 14
    }

    /// Hit-test the maximize button (middle title bar button).
    pub fn hit_maximize_button(&self, px: i32, py: i32) -> bool {
        if self.title.is_none() || self.minimized {
            return false;
        }
        let bx = self.x + self.width as i32 - 38;
        let by = self.y + 3;
        px >= bx && px < bx + 14 && py >= by && py < by + 14
    }
}
