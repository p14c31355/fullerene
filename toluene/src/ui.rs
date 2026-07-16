//! UI primitives for Toluene SDK.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowHandle(pub u64);

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

pub fn create_window(_title: &str, _width: u32, _height: u32) -> WindowHandle {
    WindowHandle(0)
}
pub fn close_window(_handle: WindowHandle) {}
pub fn set_window_title(_handle: WindowHandle, _title: &str) {}
pub fn draw_text(_handle: WindowHandle, _x: i32, _y: i32, _text: &str) {}
pub fn fill_rect(_handle: WindowHandle, _rect: Rect, _color: u32) {}

#[derive(Debug, Clone)]
pub struct DesktopIcon {
    pub name: &'static str,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub color: u32,
}
