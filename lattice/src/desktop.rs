extern crate alloc;

use crate::cursor::Cursor;
use crate::menu::PopupMenu;
use crate::scene::{DirtyRect, Scene};
use crate::window::WindowId;
use crate::wm::WindowManager;

/// Desktop session — pure state, no rendering.
///
/// `Desktop` is a **façade** that owns the `WindowManager`, `Cursor`,
/// `Taskbar`, menus, and clock.  It does NOT touch the compositor or
/// framebuffer.
///
/// To render, the kernel/runtime calls:
/// 1. `desktop.prepare_frame()` — consumes dirty rects from WM
/// 2. `desktop.scene()` — builds the compositor snapshot
///
/// ```ignore
/// desktop.prepare_frame();
/// let scene = desktop.scene();
/// compositor.render(&scene, &mut target);
/// ```
pub struct Desktop {
    pub wm: WindowManager,
    pub cursor: Cursor,
    bg_color: u32,
    pub taskbar: crate::taskbar::Taskbar,
    /// Cached dirty rects consumed from WM before building a scene.
    dirty_cache: alloc::vec::Vec<DirtyRect>,

    // ── Menu state ────────────────────────────────────────
    /// The currently visible popup menu (system menu or context menu).
    pub active_menu: Option<PopupMenu>,
    /// Whether the system menu was triggered (vs context menu).
    pub menu_is_system: bool,
    /// Cached overlay rectangles for the active menu (populated in prepare_frame).
    menu_overlays_cache: alloc::vec::Vec<crate::scene::OverlayRect>,

    // ── Clock state ────────────────────────────────────────
    /// Current clock text "HH:MM:SS".
    pub clock_text: alloc::string::String,

    // ── Cursor tracking for dirty-rect optimisation ───────
    /// Previous cursor position (tracked to invalidate cursor area only).
    prev_cursor_x: i32,
    prev_cursor_y: i32,
    /// Whether the cursor moved since last frame.
    cursor_moved: bool,
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
            taskbar: crate::taskbar::Taskbar::new(),
            dirty_cache: alloc::vec::Vec::new(),
            active_menu: None,
            menu_is_system: false,
            menu_overlays_cache: alloc::vec::Vec::new(),
            clock_text: alloc::string::String::new(),
            prev_cursor_x: 512,
            prev_cursor_y: 384,
            cursor_moved: false,
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
        // If a menu is open, check if click hits it
        if let Some(ref menu) = self.active_menu {
            let cx = self.cursor.x;
            let cy = self.cursor.y;
            if let Some(_idx) = menu.hit_test(cx, cy) {
                // Menu item clicked — handle action
                // (action dispatch is done by Solvent via process_menu_action)
                self.active_menu = None;
                return;
            }
            // Click outside menu — dismiss
            self.active_menu = None;
            return;
        }
        self.wm.on_mouse_down(self.cursor.x, self.cursor.y);
    }

    /// Show the system menu (triggered from taskbar).
    pub fn show_system_menu(&mut self) {
        let items = crate::menu::system_menu_items();
        let bar_y = 800u32.saturating_sub(crate::taskbar::TASKBAR_HEIGHT); // approximate
        self.active_menu = Some(PopupMenu::new(4, bar_y.saturating_sub(items.len() as u32 * crate::menu::ITEM_HEIGHT + 4), items));
        self.menu_is_system = true;
    }

    /// Show the context menu (right‑click on desktop).
    pub fn show_context_menu(&mut self, x: i32, y: i32) {
        let items = crate::menu::desktop_context_menu();
        let mx = (x as u32).min(1024);
        let my = (y as u32).min(768);
        self.active_menu = Some(PopupMenu::new(mx, my, items));
        self.menu_is_system = false;
    }

    /// Dismiss the active menu.
    pub fn dismiss_menu(&mut self) {
        self.active_menu = None;
    }

    /// Move mouse (drag if button held).
    pub fn mouse_move(&mut self, x: i32, y: i32) {
        // Track cursor movement for dirty-rect optimisation.
        if self.cursor.x != x || self.cursor.y != y {
            self.cursor_moved = true;
            self.prev_cursor_x = self.cursor.x;
            self.prev_cursor_y = self.cursor.y;
        }
        self.set_cursor(x, y);
        if self.active_menu.is_none() {
            self.wm.on_mouse_move(x, y);
        }
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

    /// Update the taskbar entries from the current window list.
    pub fn update_taskbar(&mut self) {
        self.taskbar.update_from_windows(self.wm.windows());
        // Update clock text on taskbar
        self.taskbar.clock_text = self.clock_text.clone();
    }

    // ── frame preparation ───────────────────────────────────

    /// Consume dirty rects from the window manager and cache them.
    ///
    /// Must be called **before** [`scene`] on each frame, so that the
    /// compositor receives the correct dirty regions.
    pub fn prepare_frame(&mut self) {
        self.dirty_cache = self.wm.consume_dirty_rects();

        // If the cursor moved, add dirty rects for the old and new
        // cursor positions (32×32 pixels each) so only the cursor
        // area is redrawn, not the entire screen.
        if self.cursor_moved {
            let cur_sz = crate::cursor::Cursor::SIZE as i32;
            let old_x = self.prev_cursor_x - crate::cursor::Cursor::HOTSPOT_X;
            let old_y = self.prev_cursor_y - crate::cursor::Cursor::HOTSPOT_Y;
            let new_x = self.cursor.x - crate::cursor::Cursor::HOTSPOT_X;
            let new_y = self.cursor.y - crate::cursor::Cursor::HOTSPOT_Y;

            self.dirty_cache.push(DirtyRect::new(
                old_x.max(0) as u32,
                old_y.max(0) as u32,
                cur_sz as u32,
                cur_sz as u32,
            ));
            if old_x != new_x || old_y != new_y {
                self.dirty_cache.push(DirtyRect::new(
                    new_x.max(0) as u32,
                    new_y.max(0) as u32,
                    cur_sz as u32,
                    cur_sz as u32,
                ));
            }
            self.cursor_moved = false;
        }

        // Generate menu overlay rects into the cache so scene() can
        // safely reference them without dangling pointers.
        self.menu_overlays_cache.clear();
        if let Some(ref menu) = self.active_menu {
            self.dirty_cache.push(DirtyRect::new(
                menu.x,
                menu.y,
                menu.width,
                menu.height,
            ));
            self.menu_overlays_cache = menu.to_overlays();
        }
    }

    // ── scene snapshot ──────────────────────────────────────

    /// Build an immutable snapshot for the compositor.
    ///
    /// Call [`prepare_frame`] first to populate the dirty rects.
    pub fn scene(&self) -> Scene<'_> {
        Scene {
            windows: self.wm.windows(),
            cursor: Some(&self.cursor),
            bg_color: self.bg_color,
            dirty_rects: &self.dirty_cache,
            taskbar: Some(&self.taskbar),
            overlays: &self.menu_overlays_cache,
            layered: true,
        }
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

        // Use a 200×200 target so the 28-pixel taskbar at the bottom
        // does not clobber the pixel at (0,0).
        dt.prepare_frame();
        let mut target = TestTarget::new(200, 200);
        let scene = dt.scene();
        Compositor::render(&scene, &mut target);

        // Top‑left corner of the window should be red.
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

    #[test]
    fn test_system_menu() {
        let mut dt = Desktop::new(0x202020);
        dt.show_system_menu();
        assert!(dt.active_menu.is_some());
        let menu = dt.active_menu.as_ref().unwrap();
        assert!(menu.items.len() >= 3);
        // Click outside dismisses
        dt.set_cursor(999, 999);
        dt.mouse_down();
        assert!(dt.active_menu.is_none());
    }

    #[test]
    fn test_context_menu() {
        let mut dt = Desktop::new(0x202020);
        dt.show_context_menu(100, 200);
        assert!(dt.active_menu.is_some());
        let menu = dt.active_menu.as_ref().unwrap();
        assert!(menu.items.len() >= 2);
        // Click on first item
        dt.set_cursor(menu.x as i32 + 4, menu.y as i32 + crate::menu::MENU_BORDER as i32 + 4);
        dt.mouse_down();
        assert!(dt.active_menu.is_none()); // dismissed after click
    }
}