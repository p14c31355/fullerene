//! UI primitives for Toluene SDK.
//!
//! Provides window management and drawing helpers for
//! user-space GUI applications.

/// Window handle type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowHandle(pub u64);

/// Rectangle type for window/surface geometry.
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// Create a new window with the given title and dimensions.
/// Returns a window handle that can be used for drawing.
pub fn create_window(title: &str, width: u32, height: u32) -> WindowHandle {
    // In the kernel context, this would call the window manager.
    // For user-space, this is a stub.
    let _ = title;
    let _ = width;
    let _ = height;
    WindowHandle(0)
}

/// Close a window.
pub fn close_window(_handle: WindowHandle) {
    // Stub
}

/// Set window title.
pub fn set_window_title(_handle: WindowHandle, _title: &str) {
    // Stub
}

/// Draw text at position in a window.
pub fn draw_text(_handle: WindowHandle, _x: i32, _y: i32, _text: &str) {
    // Stub
}

/// Fill a rectangle with a colour.
pub fn fill_rect(_handle: WindowHandle, _rect: Rect, _color: u32) {
    // Stub
}

/// Desktop icon entry.
#[derive(Debug, Clone)]
pub struct DesktopIcon {
    pub name: &'static str,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub color: u32,
}
