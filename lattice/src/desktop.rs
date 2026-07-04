extern crate alloc;

use crate::cursor::Cursor;
use crate::menu::PopupMenu;
use crate::network_menu::{self, ApDisplay, NetStatus};
use crate::scene::{DirtyRect, Scene};
use crate::window::WindowId;
use crate::wm::WindowManager;
use alloc::string::String;
use alloc::vec::Vec;

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
    /// Show the WiFi network menu.
    ShowNetworkMenu,
    /// Connect to the specified access point by index.
    ConnectAp(usize),
    /// Dismiss the password dialog.
    DismissPasswordDialog,
    /// Submit the password in the dialog.
    SubmitPassword,
    /// Add character to password input.
    PasswordChar(u8),
    /// Delete last character from password.
    PasswordBackspace,
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
            "show_network_menu" => DesktopAction::ShowNetworkMenu,
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

    // ── Network / WiFi state ─────────────────────────
    /// Whether the network menu is open.
    pub network_menu_open: bool,
    /// Cached AP list for display.
    pub ap_list: alloc::vec::Vec<ApDisplay>,
    /// Current network status.
    pub net_status: NetStatus,
    /// Position of the network menu.
    pub net_menu_x: u32,
    pub net_menu_y: u32,
    /// Password dialog state
    pub pwd_dialog_open: bool,
    pub pwd_dialog_ssid: String,
    pub pwd_dialog_password: String,
    pub pwd_dialog_cursor: usize,
    pub pwd_dialog_x: u32,
    pub pwd_dialog_y: u32,
    /// Index in ap_list of the target AP for connection.
    pub pwd_target_ap: Option<usize>,
    /// WiFi signal level for indicator (0-100).
    pub wifi_signal: u8,
    /// Whether any WiFi networks are visible.
    pub wifi_networks_visible: bool,
    /// Shift key held state for password dialog.
    pub shift_held: bool,

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
            network_menu_open: false,
            ap_list: alloc::vec::Vec::new(),
            net_status: NetStatus::NoDevice,
            net_menu_x: 0,
            net_menu_y: 0,
            pwd_dialog_open: false,
            pwd_dialog_ssid: String::new(),
            pwd_dialog_password: String::new(),
            pwd_dialog_cursor: 0,
            pwd_dialog_x: 0,
            pwd_dialog_y: 0,
            pwd_target_ap: None,
            wifi_signal: 0,
            wifi_networks_visible: false,
            shift_held: false,
        }
    }

    /// Return the usable work area (screen minus taskbar and top panel if visible).
    pub fn work_area(&self, fb_width: u32, fb_height: u32) -> (u32, u32) {
        let bar_h = crate::taskbar::TASKBAR_HEIGHT;
        let panel_h = if crate::top_panel::is_top_panel_enabled() {
            crate::top_panel::TOP_PANEL_HEIGHT
        } else {
            0
        };
        let total_h = fb_height.saturating_sub(bar_h).saturating_sub(panel_h);
        (fb_width, total_h)
    }

    /// Offset from top edge due to top panel.
    pub fn top_panel_offset(&self) -> u32 {
        if crate::top_panel::is_top_panel_enabled() {
            crate::top_panel::TOP_PANEL_HEIGHT
        } else {
            0
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
    ///
    /// `fb_width` / `fb_height` are required for maximize toggle.
    pub fn mouse_down(&mut self, fb_width: u32, fb_height: u32) {
        let cx = self.cursor.x;
        let cy = self.cursor.y;

        // ── Handle password dialog clicks ───────────────────────
        if self.pwd_dialog_open {
            // Check if click is inside dialog
            let in_dialog = cx >= self.pwd_dialog_x as i32
                && cx < (self.pwd_dialog_x + network_menu::PWD_DIALOG_W) as i32
                && cy >= self.pwd_dialog_y as i32
                && cy < (self.pwd_dialog_y + network_menu::PWD_DIALOG_H) as i32;

            if in_dialog {
                // "Connect" button area (bottom right)
                let btn_w = 80i32;
                let btn_h = 24i32;
                let btn_x = (self.pwd_dialog_x + network_menu::PWD_DIALOG_W - btn_w as u32 - 20) as i32;
                let btn_y = (self.pwd_dialog_y + network_menu::PWD_DIALOG_H - btn_h as u32 - 10) as i32;
                let cancel_x = btn_x - btn_w - 10;

                if cx >= btn_x && cx < btn_x + btn_w && cy >= btn_y && cy < btn_y + btn_h {
                    self.menu_action_pending = Some(DesktopAction::SubmitPassword);
                } else if cx >= cancel_x && cx < cancel_x + btn_w && cy >= btn_y && cy < btn_y + btn_h {
                    self.menu_action_pending = Some(DesktopAction::DismissPasswordDialog);
                }

                self.push_dirty_rect(crate::scene::DirtyRect::new(
                    self.pwd_dialog_x, self.pwd_dialog_y,
                    network_menu::PWD_DIALOG_W, network_menu::PWD_DIALOG_H,
                ));
                return;
            } else {
                // Click outside dialog - dismiss
                self.pwd_dialog_open = false;
                self.pwd_target_ap = None;
                self.shift_held = false;
                self.push_dirty_rect(crate::scene::DirtyRect::new(
                    self.pwd_dialog_x, self.pwd_dialog_y,
                    network_menu::PWD_DIALOG_W, network_menu::PWD_DIALOG_H,
                ));
                self.dismiss_network_menu();
                return;
            }
        }

        // ── Handle network menu clicks ─────────────────────────
        if self.network_menu_open {
            // Check if click hits an AP entry
            if let Some(ap_idx) = network_menu::hit_ap_entry(
                cx, cy, self.net_menu_x, self.net_menu_y, self.ap_list.len(),
            ) {
                if ap_idx < self.ap_list.len() {
                    let ap = &self.ap_list[ap_idx];
                    if ap.has_lock {
                        // Open password dialog
                        self.pwd_dialog_open = true;
                        self.pwd_dialog_ssid = ap.ssid.clone();
                        self.pwd_dialog_password = String::new();
                        self.pwd_dialog_cursor = 0;
                        self.shift_held = false;
                        self.pwd_target_ap = Some(ap_idx);
                        self.pwd_dialog_x = (fb_width / 2).saturating_sub(network_menu::PWD_DIALOG_W / 2);
                        self.pwd_dialog_y = (fb_height / 2).saturating_sub(network_menu::PWD_DIALOG_H / 2);
                    } else {
                        // Open AP - connect directly
                        self.menu_action_pending = Some(DesktopAction::ConnectAp(ap_idx));
                    }
                }
                let menu_h = 4 + (self.ap_list.len() + 1) as u32 * network_menu::NET_MENU_ITEM_HEIGHT;
                self.wm.dirty_rects.push(crate::scene::DirtyRect::new(
                    self.net_menu_x, self.net_menu_y,
                    network_menu::NET_MENU_WIDTH, menu_h,
                ));
                self.network_menu_open = false;
                return;
            }

            // Click outside - dismiss
            self.dismiss_network_menu();
            return;
        }

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

        // Check WiFi icon click (before taskbar window check)
        let wifi_icon_x = self.taskbar.wifi_icon_x(fb_width);
        if network_menu::hit_wifi_icon(self.cursor.x, self.cursor.y, fb_width, fb_height, wifi_icon_x) {
            self.menu_action_pending = Some(DesktopAction::ShowNetworkMenu);
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
                let wy = self.top_panel_offset() as i32;
                self.wm.toggle_maximize(id, 0, wy, ww, wh);
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

    /// Show the network menu with access point list.
    pub fn show_network_menu(&mut self, fb_width: u32, fb_height: u32) {
        self.network_menu_open = true;
        // Position the menu above the WiFi icon, right-aligned to stay on-screen
        let wifi_icon_x = self.taskbar.wifi_icon_x(fb_width);
        // Right-align the menu with the WiFi icon so it doesn't extend past fb_width
        self.net_menu_x = if wifi_icon_x + network_menu::NET_MENU_WIDTH > fb_width {
            fb_width.saturating_sub(network_menu::NET_MENU_WIDTH)
        } else {
            wifi_icon_x
        };
        let menu_h = 4 + (self.ap_list.len() + 1) as u32 * network_menu::NET_MENU_ITEM_HEIGHT;
        self.net_menu_y = fb_height.saturating_sub(crate::taskbar::TASKBAR_HEIGHT).saturating_sub(menu_h);

        self.push_dirty_rect(crate::scene::DirtyRect::new(
            self.net_menu_x, self.net_menu_y,
            network_menu::NET_MENU_WIDTH, menu_h,
        ));
    }

    /// Dismiss the network menu.
    pub fn dismiss_network_menu(&mut self) {
        if self.network_menu_open {
            let menu_h = 4 + (self.ap_list.len() + 1) as u32 * network_menu::NET_MENU_ITEM_HEIGHT;
            self.push_dirty_rect(crate::scene::DirtyRect::new(
                self.net_menu_x, self.net_menu_y,
                network_menu::NET_MENU_WIDTH, menu_h,
            ));
            self.network_menu_open = false;
        }
    }

    /// Update the access point list for the network menu.
    /// Returns `true` if the list or status actually changed.
    pub fn update_ap_list(&mut self, aps: alloc::vec::Vec<ApDisplay>, status: NetStatus) -> bool {
        let changed = self.ap_list != aps || self.net_status != status;
        if changed {
            self.ap_list = aps;
            self.net_status = status;
            self.wifi_networks_visible = match &self.net_status {
                NetStatus::NoDevice => false,
                _ => true,
            };
        }
        changed
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

    /// Re-layout all maximized windows using current work area and panel offset.
    ///
    /// Called after the top panel setting changes to ensure maximized windows
    /// are repositioned to match the new panel state.
    pub fn relayout_maximized_windows(&mut self, fb_width: u32, fb_height: u32) {
        let (ww, wh) = self.work_area(fb_width, fb_height);
        let wy = self.top_panel_offset() as i32;
        let mut dirty_rects = Vec::new();
        for w in self.wm.windows_mut().iter_mut() {
            if w.maximized {
                w.x = 0;
                w.y = wy;
                w.width = ww;
                w.height = wh;
                dirty_rects.push(crate::wm::window_dirty_rect(w));
            }
        }
        self.wm.dirty_rects.extend(dirty_rects);
    }

    /// Update the taskbar entries from the current window list.
    ///
    /// Returns `true` when the entry list changed (count or order).
    pub fn update_taskbar(&mut self) -> bool {
        let prev_count = self.taskbar.entries.len();
        self.taskbar.update_from_windows(self.wm.windows());
        // Update clock text on taskbar
        self.taskbar.clock_text = self.clock_text.clone();
        // Update WiFi state on taskbar
        self.taskbar.wifi_connected = matches!(&self.net_status, NetStatus::Connected(_, _));
        self.taskbar.wifi_visible = self.wifi_networks_visible;
        self.taskbar.wifi_signal = self.wifi_signal;
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

        // Network menu overlay (rendered by compositor via scene.active_menu render)
        if self.network_menu_open {
            let menu_h = 4 + (self.ap_list.len() + 1) as u32 * network_menu::NET_MENU_ITEM_HEIGHT;
            self.dirty_cache.push(DirtyRect::new(
                self.net_menu_x, self.net_menu_y,
                network_menu::NET_MENU_WIDTH, menu_h,
            ));
            // Also push WiFi icon area as dirty
            let wifi_icon_x = self.taskbar.wifi_icon_x(fb_width);
            self.dirty_cache.push(DirtyRect::new(
                wifi_icon_x,
                fb_height.saturating_sub(crate::taskbar::TASKBAR_HEIGHT),
                network_menu::NET_ICON_WIDTH,
                crate::taskbar::TASKBAR_HEIGHT,
            ));
        }

        // Password dialog overlay
        if self.pwd_dialog_open {
            self.dirty_cache.push(DirtyRect::new(
                self.pwd_dialog_x, self.pwd_dialog_y,
                network_menu::PWD_DIALOG_W, network_menu::PWD_DIALOG_H,
            ));
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
            network_menu_open: self.network_menu_open,
            net_menu_x: self.net_menu_x,
            net_menu_y: self.net_menu_y,
            net_aps: &self.ap_list,
            net_status: &self.net_status,
            pwd_dialog_open: self.pwd_dialog_open,
            pwd_dialog_x: self.pwd_dialog_x,
            pwd_dialog_y: self.pwd_dialog_y,
            pwd_ssid: &self.pwd_dialog_ssid,
            pwd_password: &self.pwd_dialog_password,
            pwd_cursor: self.pwd_dialog_cursor,
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
