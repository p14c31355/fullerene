extern crate alloc;

use crate::cursor::Cursor;
use crate::scene::Scene;
use crate::window::WindowId;
use crate::wm::WindowManager;

/// Desktop session — pure state, no rendering.
///
/// `Desktop` is a **façade** that owns the `WindowManager` and `Cursor`.
/// It does NOT touch the compositor or framebuffer.
///
/// To render, the kernel/runtime calls `desktop.scene()` and passes the
/// resulting `Scene` to the compositor:
///
/// ```ignore
/// let scene = desktop.scene();
/// compositor.render(&scene, &mut target);
/// ```
pub struct Desktop {
    pub wm: WindowManager,
    pub cursor: Cursor,
    bg_color: u32,
}

impl Desktop {
    /// Create a new desktop with a given background colour.
    ///
    /// The cursor starts at screen centre and is visible by default.
    pub fn new(bg_color: u32) -> Self {
        let mut cursor = Cursor::new(512, 384);
        cursor.visible = true;
        Self {
            wm: WindowManager::new(),
            cursor,
            bg_color,
        }
    }

    // ── convenience delegates ───────────────────────────────

    pub fn create_window(&mut self, x: i32, y: i32, w: u32, h: u32, color: u32) -> WindowId {
        self.wm.create_window(x, y, w, h, color)
    }

    pub fn remove_window(&mut self, id: WindowId) -> bool {
        self.wm.remove_window(id)
    }

    /// Move the cursor (makes it visible).
    pub fn set_cursor(&mut self, x: i32, y: i32) {
        self.cursor.x = x;
        self.cursor.y = y;
        self.cursor.visible = true;
    }

    /// Press mouse button at current cursor position.
    pub fn mouse_down(&mut self) {
        self.wm.on_mouse_down(self.cursor.x, self.cursor.y);
    }

    /// Move mouse (drag if button held).
    pub fn mouse_move(&mut self, x: i32, y: i32) {
        self.set_cursor(x, y);
        self.wm.on_mouse_move(x, y);
    }

    /// Release mouse button.
    pub fn mouse_up(&mut self) {
        self.wm.on_mouse_up();
    }

    /// Hide the cursor.
    pub fn hide_cursor(&mut self) {
        self.cursor.visible = false;
    }

    /// Show the cursor.
    pub fn show_cursor(&mut self) {
        self.cursor.visible = true;
    }

    // ── scene snapshot ──────────────────────────────────────

    /// Build an immutable snapshot for the compositor.
    ///
    /// This is the **only** bridge between state and rendering.
    pub fn scene(&self) -> Scene<'_> {
        Scene::new(self.wm.windows(), Some(&self.cursor), self.bg_color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::{Compositor, RenderTarget};
    use alloc::vec::Vec;
    use core::iter;

    struct TestTarget {
        pixels: Vec<u32>,
        w: u32,
        h: u32,
    }

    impl RenderTarget for TestTarget {
        fn buffer(&mut self) -> &mut [u32] {
            &mut self.pixels
        }
        fn dimensions(&self) -> (u32, u32) {
            (self.w, self.h)
        }
    }

    impl TestTarget {
        fn new(w: u32, h: u32) -> Self {
            Self {
                pixels: iter::repeat(0u32).take((w * h) as usize).collect(),
                w,
                h,
            }
        }
    }

    #[test]
    fn test_desktop_creates_windows() {
        let mut dt = Desktop::new(0x202020);
        let id = dt.create_window(0, 0, 50, 50, 0xFFFFFF);
        assert!(dt.wm.window_at(10, 10) == Some(id));
    }

    #[test]
    fn test_desktop_render() {
        let mut dt = Desktop::new(0x202020);
        dt.create_window(0, 0, 100, 100, 0xFF0000);

        let mut target = TestTarget::new(20, 20);
        let scene = dt.scene();
        Compositor::render(&scene, &mut target);

        // Top‑left of the window should be red
        assert_eq!(target.pixels[0], 0xFF0000);
    }

    #[test]
    fn test_desktop_mouse_drag() {
        let mut dt = Desktop::new(0x202020);
        // Create a titled window so drag via title bar works
        let id = dt.wm.create_titled_window(10, 10, 100, 100, 0xFF0000, "Test");

        // Click title bar at (50, 20) — y=20 is inside title bar (10..30)
        dt.set_cursor(50, 20);
        dt.mouse_down();

        // Drag to (100, 50)
        dt.mouse_move(100, 50);

        let win = dt.wm.windows().iter().find(|w| w.id == id).unwrap();
        // offset = (50-10, 20-10) = (40, 10), new pos = (100-40, 50-10) = (60, 40)
        assert_eq!(win.x, 60);
        assert_eq!(win.y, 40);

        dt.mouse_up();
    }
}