use crate::cursor::Cursor;
use crate::window::Window;

/// A rectangular dirty/update region in pixel coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirtyRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl DirtyRect {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
    pub const fn full(fb_width: u32, fb_height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width: fb_width,
            height: fb_height,
        }
    }
    pub fn intersects(&self, other: &DirtyRect) -> bool {
        self.x < other.x + other.width
            && self.x + self.width > other.x
            && self.y < other.y + other.height
            && self.y + self.height > other.y
    }
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

/// Layer separation: windows, overlays, and system UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    /// Desktop background (wallpaper/solid color).
    Desktop = 0,
    /// Application windows.
    Window = 1,
    /// Overlay elements (menus, tooltips, popups).
    Overlay = 2,
    /// System UI (cursor, FPS overlay, taskbar).
    System = 3,
}

/// An immutable snapshot of the desktop state for the compositor.
#[derive(Clone)]
pub struct Scene<'a> {
    pub windows: &'a [Window],
    pub cursor: Option<&'a Cursor>,
    pub bg_color: u32,
    pub dirty_rects: &'a [DirtyRect],

    /// Optional taskbar reference for rendering.
    pub taskbar: Option<&'a crate::taskbar::Taskbar>,

    /// Overlay elements to draw above windows but below system UI.
    pub overlays: &'a [OverlayRect],

    /// Desktop icons (drawn on the background layer, behind windows).
    pub desktop_icons: Option<&'a crate::desktop_icons::DesktopIconLayer>,

    /// Whether to use layer-based rendering order.
    pub layered: bool,
}

/// A simple overlay rectangle (e.g. right-click menu, dropdown).
#[derive(Clone, Copy, Debug)]
pub struct OverlayRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub color: u32,
}

impl<'a> Scene<'a> {
    pub fn new(windows: &'a [Window], cursor: Option<&'a Cursor>, bg_color: u32) -> Self {
        Self {
            windows,
            cursor,
            bg_color,
            dirty_rects: &[],
            taskbar: None,
            overlays: &[],
            desktop_icons: None,
            layered: false,
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
            taskbar: None,
            overlays: &[],
            desktop_icons: None,
            layered: false,
        }
    }

    pub fn with_taskbar(mut self, taskbar: &'a crate::taskbar::Taskbar) -> Self {
        self.taskbar = Some(taskbar);
        self
    }

    pub fn with_overlays(mut self, overlays: &'a [OverlayRect]) -> Self {
        self.overlays = overlays;
        self
    }

    pub fn with_layered(mut self, layered: bool) -> Self {
        self.layered = layered;
        self
    }
}
