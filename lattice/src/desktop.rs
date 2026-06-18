extern crate alloc;

use crate::cursor::Cursor;
use crate::menu::PopupMenu;
use crate::scene::{DirtyRect, Scene};
use crate::window::WindowId;
use crate::wm::WindowManager;

/// Actions that can be dispatched from desktop menus (context menu, system menu, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopAction {
    NewTerminal,
    NewShell,
    TaskManager,
    DeviceManager,
    FileManager,
    ToggleTiling,
    Refresh,
    About,
    SysInfo,
    Shutdown,
    Reboot,
    Separator,
    ChangeWallpaperSettings,
    OpenEditor,
}

impl DesktopAction {
    /// Parse an action string from a menu item into a `DesktopAction`.
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "new_terminal" => DesktopAction::NewTerminal,
            "new_shell" => DesktopAction::NewShell,
            "task_manager" => DesktopAction::TaskManager,
            "device_manager" => DesktopAction::DeviceManager,
            "file_manager" => DesktopAction::FileManager,
            "toggle_tiling" => DesktopAction::ToggleTiling,
            "refresh" => DesktopAction::Refresh,
            "about" => DesktopAction::About,
            "sysinfo" => DesktopAction::SysInfo,
            "shutdown" => DesktopAction::Shutdown,
            "reboot" => DesktopAction::Reboot,
            "separator" => DesktopAction::Separator,
            "change_wallpaper" => DesktopAction::ChangeWallpaperSettings,
            "open_editor" => DesktopAction::OpenEditor,
            _ => return None,
        })
    }
}

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

    /// Whether the next frame should redraw the entire screen.
    ///
    /// Set to `true` on construction so the very first frame initialises
    /// the whole framebuffer with the desktop background colour.  Without
    /// this the compositor only draws the terminal window and cursor
    /// dirty rects, leaving the rest of the screen uninitialised (which
    /// manifests as a "paint‑by‑mouse" effect).
    needs_full_redraw: bool,

    // ── Menu state ────────────────────────────────────────
    /// The currently visible popup menu (system menu or context menu).
    pub active_menu: Option<PopupMenu>,
    /// Whether the system menu was triggered (vs context menu).
    pub menu_is_system: bool,
    /// Cached overlay rectangles for the active menu (populated in prepare_frame).
    menu_overlays_cache: alloc::vec::Vec<crate::scene::OverlayRect>,

    /// The action of the most recently clicked menu item.
    /// Cleared after being consumed by the runtime.
    pub menu_action_pending: Option<DesktopAction>,

    // ── Clock state ────────────────────────────────────────
    /// Current clock text "HH:MM:SS".
    pub clock_text: alloc::string::String,

    // ── Desktop icons (Xfce-style) ─────────────────────
    pub desktop_icons: crate::desktop_icons::DesktopIconLayer,

    // ── Top panel (GNOME-style) ─────────────────────────
    pub top_panel: crate::top_panel::TopPanel,

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
            menu_action_pending: None,
            clock_text: alloc::string::String::new(),
            desktop_icons: crate::desktop_icons::DesktopIconLayer::new(),
            top_panel: crate::top_panel::TopPanel::new(),
            prev_cursor_x: 512,
            prev_cursor_y: 384,
            cursor_moved: false,
            needs_full_redraw: true,
        }
    }

    /// Return the usable work area (screen minus taskbar).
    pub fn work_area(&self, fb_width: u32, fb_height: u32) -> (u32, u32) {
        let bar_h = crate::taskbar::TASKBAR_HEIGHT;
        (fb_width, fb_height.saturating_sub(bar_h))
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
    ///
    /// `fb_width` / `fb_height` are required for maximize toggle.
    pub fn mouse_down(&mut self, fb_width: u32, fb_height: u32) {
        // If a menu is open, check if click hits it
        if let Some(ref menu) = self.active_menu {
            let cx = self.cursor.x;
            let cy = self.cursor.y;
            // Capture menu bounds before dismissing (needed for dirty rect)
            let menu_x = menu.x;
            let menu_y = menu.y;
            let menu_w = menu.width;
            let menu_h = menu.height;

            if let Some(idx) = menu.hit_test(cx, cy) {
                // Menu item clicked — capture action for the runtime
                if idx < menu.items.len() {
                    self.menu_action_pending = DesktopAction::from_str(&menu.items[idx].action);
                }
                self.active_menu = None;
                // Push dirty rect so compositor redraws the old menu area
                self.wm
                    .dirty_rects
                    .push(crate::scene::DirtyRect::new(menu_x, menu_y, menu_w, menu_h));
                return;
            }
            // Click outside menu — dismiss
            self.active_menu = None;
            // Push dirty rect so compositor redraws the old menu area
            self.wm
                .dirty_rects
                .push(crate::scene::DirtyRect::new(menu_x, menu_y, menu_w, menu_h));
            return;
        }

        // Check taskbar clicks first — restore minimized windows or focus.
        if let Some(tb_id) = self.taskbar_window_at(self.cursor.x, self.cursor.y, fb_height) {
            // Find the window. If minimized, restore it. Otherwise just focus.
            if let Some(w) = self.wm.windows().iter().find(|w| w.id == tb_id) {
                if w.minimized {
                    self.wm.restore_window(tb_id);
                } else {
                    self.wm.raise_to_top(tb_id);
                }
            }
            return;
        }

        // Check title bar buttons first (topmost window with title bar hit)
        for window in self.wm.windows().iter().rev() {
            if window.minimized {
                continue;
            }
            let id = window.id;
            if window.hit_close_button(self.cursor.x, self.cursor.y) {
                self.wm.close_window(id);
                // Push a dirty rect for the entire taskbar area so the
                // compositor redraws the taskbar (removing the stale button).
                self.wm.dirty_rects.push(crate::scene::DirtyRect::new(
                    0,
                    fb_height.saturating_sub(crate::taskbar::TASKBAR_HEIGHT),
                    fb_width,
                    crate::taskbar::TASKBAR_HEIGHT,
                ));
                return;
            }
            if window.hit_minimize_button(self.cursor.x, self.cursor.y) {
                self.wm.minimize_window(id);
                // Push a dirty rect for the entire taskbar area so the
                // compositor redraws the taskbar (updating button states).
                self.wm.dirty_rects.push(crate::scene::DirtyRect::new(
                    0,
                    fb_height.saturating_sub(crate::taskbar::TASKBAR_HEIGHT),
                    fb_width,
                    crate::taskbar::TASKBAR_HEIGHT,
                ));
                return;
            }
            if window.hit_maximize_button(self.cursor.x, self.cursor.y) {
                let (ww, wh) = self.work_area(fb_width, fb_height);
                self.wm.toggle_maximize(id, ww, wh);
                return;
            }
        }

        self.wm.on_mouse_down(self.cursor.x, self.cursor.y);
    }

    /// Force a full-screen redraw on the next frame.
    ///
    /// Useful when overlay modes (TaskOverview / AppGrid) need every frame
    /// to be fully recomposited rather than incremental dirty-rect updates.
    pub fn force_full_redraw(&mut self) {
        self.needs_full_redraw = true;
    }

    /// Show the system menu (triggered from taskbar).
    pub fn show_system_menu(&mut self) {
        let items = crate::menu::system_menu_items();
        let bar_y = 800u32.saturating_sub(crate::taskbar::TASKBAR_HEIGHT); // approximate
        self.active_menu = Some(PopupMenu::new(
            4,
            bar_y.saturating_sub(items.len() as u32 * crate::menu::ITEM_HEIGHT + 4),
            items,
        ));
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

    /// Check if a point (fb pixel coords) hits a taskbar button.
    ///
    /// Returns the `WindowId` of the taskbar entry whose button
    /// contains the point, or `None`.
    pub fn taskbar_window_at(&self, px: i32, py: i32, fb_height: u32) -> Option<WindowId> {
        let bar_y = fb_height.saturating_sub(crate::taskbar::TASKBAR_HEIGHT) as i32;
        if py < bar_y {
            return None;
        }
        // Simple linear scan matching the taskbar render layout.
        let btn_w = 120i32;
        let btn_h = (crate::taskbar::TASKBAR_HEIGHT - 6) as i32;
        let btn_y = bar_y + 3;
        if py < btn_y || py >= btn_y + btn_h {
            return None;
        }
        let mut btn_x = 4i32;
        for entry in self.taskbar.entries.iter() {
            let bx_end = btn_x + btn_w as i32;
            if px >= btn_x && px < bx_end {
                return Some(entry.id);
            }
            btn_x = bx_end + 4;
        }
        None
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

    /// Invalidate the dirty rect for a specific window (by id).
    ///
    /// Called from Solvent when the terminal buffer changes so that the
    /// compositor knows to redraw the window area in the next frame.
    pub fn invalidate_window(&mut self, id: WindowId) {
        if let Some(w) = self.wm.windows().iter().find(|w| w.id == id) {
            self.wm.dirty_rects.push(crate::wm::window_dirty_rect(w));
        }
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
    ///
    /// Returns `true` when the entry list changed (count or order).
    pub fn update_taskbar(&mut self) -> bool {
        let prev_count = self.taskbar.entries.len();
        self.taskbar.update_from_windows(self.wm.windows());
        // Update clock text on taskbar
        self.taskbar.clock_text = self.clock_text.clone();
        let new_count = self.taskbar.entries.len();
        new_count != prev_count
    }

    // ── frame preparation ───────────────────────────────────

    /// Push a dirty rect into the window manager queue.
    ///
    /// Use this to notify the compositor of regions that need repainting
    /// (e.g. clock change → taskbar area).
    pub fn push_dirty_rect(&mut self, rect: crate::scene::DirtyRect) {
        self.wm.dirty_rects.push(rect);
    }

    /// Returns `true` when the cached dirty-rect list is non-empty,
    /// i.e. the compositor has at least one region to repaint.
    ///
    /// Call after [`prepare_frame`] to decide whether a full compositor
    /// pass is required.
    pub fn has_pending_dirty_rects(&self) -> bool {
        !self.dirty_cache.is_empty()
    }

    /// Consume dirty rects from the window manager and cache them.
    ///
    /// Must be called **before** [`scene`] on each frame, so that the
    /// compositor receives the correct dirty regions.
    ///
    /// `fb_width` / `fb_height` are needed so that the very first frame
    /// can push a full‑screen dirty rect (see [`needs_full_redraw`]).
    pub fn prepare_frame(&mut self, fb_width: u32, fb_height: u32) {
        self.dirty_cache = self.wm.consume_dirty_rects();

        // First frame: invalidate the entire screen so the compositor
        // fills every pixel with the desktop background colour.
        if self.needs_full_redraw {
            self.dirty_cache.push(DirtyRect::full(fb_width, fb_height));
            self.needs_full_redraw = false;
        }

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
            self.dirty_cache
                .push(DirtyRect::new(menu.x, menu.y, menu.width, menu.height));
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
            // Cursor is drawn by solvent::render() via
            // `draw_cursor_direct` as the final layer.
            // Including it here would cause the compositor to
            // render it into the back‑buffer, which then gets
            // captured by the lightweight‑update save buffer,
            // producing ghost cursors after overlay transitions.
            cursor: None,
            bg_color: self.bg_color,
            dirty_rects: &self.dirty_cache,
            taskbar: Some(&self.taskbar),
            overlays: &self.menu_overlays_cache,
            desktop_icons: Some(&self.desktop_icons),
            active_menu: self.active_menu.as_ref(),
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
        dt.prepare_frame(200, 200);
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
        let id = dt
            .wm
            .create_titled_window(10, 10, 100, 100, 0xFF0000, "Test");

        // Click title bar at (50, 20) — y=20 is inside title bar (10..30)
        dt.set_cursor(50, 20);
        dt.mouse_down(1024, 768);

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
        dt.mouse_down(1024, 768);
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
        dt.set_cursor(
            menu.x as i32 + 4,
            menu.y as i32 + crate::menu::MENU_BORDER as i32 + 4,
        );
        dt.mouse_down(1024, 768);
        assert!(dt.active_menu.is_none()); // dismissed after click
    }
}
