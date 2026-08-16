extern crate alloc;

use crate::cursor::Cursor;
use crate::menu::{ITEM_HEIGHT, MENU_BORDER, PopupMenu};
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
    TaskManager,
    DeviceManager,
    FileManager,
    LogViewer,
    /// Live-updating kernel log viewer.
    KLogLive,
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
    /// Show the taskbar power menu.
    ShowPowerMenu,
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
            "task_manager" => DesktopAction::TaskManager,
            "device_manager" => DesktopAction::DeviceManager,
            "file_manager" => DesktopAction::FileManager,
            "log_viewer" => DesktopAction::LogViewer,
            "klog_live" => DesktopAction::KLogLive,
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
            "show_power_menu" => DesktopAction::ShowPowerMenu,
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
    /// Currently highlighted access point in the network menu.
    pub net_selected_idx: Option<usize>,
    /// Number of AP rows that fit in the current network menu.
    pub net_visible_rows: usize,
    /// First AP row currently shown in the network menu.
    pub net_scroll_offset: usize,
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
            net_selected_idx: None,
            net_visible_rows: 1,
            net_scroll_offset: 0,
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
        let area = crate::style::style_for(crate::style::variant()).layout_work_area(
            crate::common::Rect {
                x: 0,
                y: 0,
                width: fb_width,
                height: fb_height,
            },
        );
        (area.width, area.height)
    }

    /// Offset from top edge due to top panel.
    pub fn top_panel_offset(&self) -> u32 {
        crate::style::style_for(crate::style::variant())
            .layout_work_area(crate::common::Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            })
            .y
            .max(0) as u32
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
    pub fn mouse_down(&mut self, fb_width: u32, fb_height: u32) -> Option<WindowId> {
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
                let btn_x =
                    (self.pwd_dialog_x + network_menu::PWD_DIALOG_W - btn_w as u32 - 20) as i32;
                let btn_y =
                    (self.pwd_dialog_y + network_menu::PWD_DIALOG_H - btn_h as u32 - 10) as i32;
                let cancel_x = btn_x - btn_w - 10;

                if cx >= btn_x && cx < btn_x + btn_w && cy >= btn_y && cy < btn_y + btn_h {
                    self.menu_action_pending = Some(DesktopAction::SubmitPassword);
                } else if cx >= cancel_x
                    && cx < cancel_x + btn_w
                    && cy >= btn_y
                    && cy < btn_y + btn_h
                {
                    self.menu_action_pending = Some(DesktopAction::DismissPasswordDialog);
                }

                self.push_dirty_rect(crate::scene::DirtyRect::new(
                    self.pwd_dialog_x,
                    self.pwd_dialog_y,
                    network_menu::PWD_DIALOG_W,
                    network_menu::PWD_DIALOG_H,
                ));
                return None;
            } else {
                // Click outside dialog - dismiss
                self.dismiss_password_dialog();
                self.dismiss_network_menu();
                return None;
            }
        }

        // ── Handle network menu clicks ─────────────────────────
        if self.network_menu_open {
            // Check if click hits an AP entry
            if let Some(ap_idx) = network_menu::hit_ap_entry(
                cx,
                cy,
                self.net_menu_x,
                self.net_menu_y,
                self.ap_list.len(),
                self.net_visible_rows,
                self.net_scroll_offset,
            ) {
                if ap_idx < self.ap_list.len() {
                    self.net_selected_idx = Some(ap_idx);
                    self.activate_network_ap(ap_idx, fb_width, fb_height);
                }
                return None;
            }

            // Click outside - dismiss
            self.dismiss_network_menu();
            return None;
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
                return None;
            }
            // Click outside menu — dismiss
            self.active_menu = None;
            // Push dirty rect so compositor redraws the old menu area
            self.wm
                .dirty_rects
                .push(crate::scene::DirtyRect::new(menu_x, menu_y, menu_w, menu_h));
            return None;
        }

        // Check WiFi icon click (before taskbar window check)
        let wifi_icon_x = self.taskbar.wifi_icon_x(fb_width);
        if network_menu::hit_wifi_icon(
            self.cursor.x,
            self.cursor.y,
            fb_width,
            fb_height,
            wifi_icon_x,
        ) {
            self.menu_action_pending = Some(DesktopAction::ShowNetworkMenu);
            return None;
        }

        // The power control sits between WiFi and the clock.
        let power_icon_x = self.taskbar.power_icon_x(fb_width);
        let bar_y = fb_height.saturating_sub(crate::style::taskbar_height()) as i32;
        if self.cursor.x >= power_icon_x as i32
            && self.cursor.x < (power_icon_x + crate::taskbar::POWER_STATUS_WIDTH) as i32
            && self.cursor.y >= bar_y
        {
            self.menu_action_pending = Some(DesktopAction::ShowPowerMenu);
            return None;
        }

        // Check taskbar clicks first — restore minimized windows or focus.
        if let Some(tb_id) =
            self.taskbar_window_at(self.cursor.x, self.cursor.y, fb_width, fb_height)
        {
            // Find the window. If minimized, restore it. Otherwise just focus.
            if let Some(w) = self.wm.windows().iter().find(|w| w.id == tb_id) {
                if w.minimized {
                    self.wm.restore_window(tb_id);
                } else {
                    self.wm.raise_to_top(tb_id);
                }
            }
            return None;
        }

        // Check title bar buttons first (topmost window with title bar hit)
        let style = crate::style::style_for(crate::style::variant());
        for window in self.wm.windows().iter().rev() {
            if window.minimized {
                continue;
            }
            let id = window.id;
            match style.hit_test_chrome(
                window,
                crate::common::Point {
                    x: self.cursor.x,
                    y: self.cursor.y,
                },
            ) {
                crate::common::ChromeHit::Close => {
                    self.wm.close_window(id);
                    self.wm.dirty_rects.push(crate::scene::DirtyRect::new(
                        0,
                        fb_height.saturating_sub(crate::style::taskbar_height()),
                        fb_width,
                        crate::style::taskbar_height(),
                    ));
                    return Some(id);
                }
                crate::common::ChromeHit::Minimize => {
                    self.wm.minimize_window(id);
                    self.wm.dirty_rects.push(crate::scene::DirtyRect::new(
                        0,
                        fb_height.saturating_sub(crate::style::taskbar_height()),
                        fb_width,
                        crate::style::taskbar_height(),
                    ));
                    return None;
                }
                crate::common::ChromeHit::Maximize => {
                    let (ww, wh) = self.work_area(fb_width, fb_height);
                    let wy = self.top_panel_offset() as i32;
                    self.wm.toggle_maximize(id, 0, wy, ww, wh);
                    return None;
                }
                _ => {}
            }
        }

        self.wm.on_mouse_down(self.cursor.x, self.cursor.y);
        None
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
        let bar_y = 800u32.saturating_sub(crate::style::taskbar_height()); // approximate
        self.active_menu = Some(PopupMenu::new(
            4,
            bar_y.saturating_sub(items.len() as u32 * crate::menu::ITEM_HEIGHT + 4),
            items,
        ));
        self.menu_is_system = true;
    }

    /// Show the power menu above the taskbar power button.
    pub fn show_power_menu(&mut self, fb_width: u32, fb_height: u32) {
        let items = crate::menu::power_menu_items();
        let mut menu = PopupMenu::new(0, 0, items);
        let usable_height = fb_height.saturating_sub(crate::style::taskbar_height());
        if menu.height > usable_height {
            let max_items = usable_height.saturating_sub(MENU_BORDER * 2) / ITEM_HEIGHT;
            menu.items.truncate(max_items as usize);
            menu.height =
                (menu.items.len() as u32 * ITEM_HEIGHT + MENU_BORDER * 2).min(usable_height);
        }
        if fb_width < MENU_BORDER * 2 {
            // There is no scanout width in which a non-empty menu can draw
            // both borders safely. Keep the popup object bounded but hide it
            // instead of allowing PopupMenu::to_overlays to underflow.
            menu.items.clear();
            menu.width = fb_width;
            menu.height = 0;
            menu.visible = false;
        } else {
            menu.width = menu.width.min(fb_width).max(MENU_BORDER * 2);
        }
        let button_x = self.taskbar.power_icon_x(fb_width);
        let x = button_x
            .saturating_add(crate::taskbar::POWER_STATUS_WIDTH)
            .saturating_sub(menu.width)
            .min(fb_width.saturating_sub(menu.width));
        let y = usable_height.saturating_sub(menu.height.saturating_add(4));
        self.active_menu = Some(PopupMenu { x, y, ..menu });
        self.menu_is_system = true;
    }

    /// Show the context menu (right‑click on desktop).
    pub fn show_context_menu(&mut self, x: i32, y: i32) {
        self.show_context_menu_in_bounds(x, y, 1024, 768);
    }

    /// Show the desktop menu without allowing its popup to extend beyond
    /// the actual scanout.  The old fixed 1024×768 clamp made the menu
    /// partly unreachable on smaller displays and was especially confusing
    /// when the framebuffer was negotiated after desktop initialisation.
    pub fn show_context_menu_in_bounds(&mut self, x: i32, y: i32, fb_width: u32, fb_height: u32) {
        let items = crate::menu::desktop_context_menu();
        let menu = PopupMenu::new(0, 0, items);
        let mx = (x.max(0) as u32).min(fb_width.saturating_sub(menu.width));
        let my = (y.max(0) as u32).min(fb_height.saturating_sub(menu.height));
        self.active_menu = Some(PopupMenu {
            x: mx,
            y: my,
            ..menu
        });
        self.menu_is_system = false;
    }

    /// Show the network menu with access point list.
    pub fn show_network_menu(&mut self, fb_width: u32, fb_height: u32) {
        self.network_menu_open = true;
        self.net_selected_idx = (!self.ap_list.is_empty()).then_some(0);
        self.net_scroll_offset = 0;
        self.update_network_menu_geometry(fb_width, fb_height);
        self.push_dirty_rect(crate::scene::DirtyRect::new(
            self.net_menu_x,
            self.net_menu_y,
            network_menu::NET_MENU_WIDTH,
            network_menu::menu_height(self.net_visible_rows),
        ));
    }

    /// Recalculate the bounded network-menu rectangle for the current scanout.
    ///
    /// The status row is always kept visible. AP rows use the remaining space
    /// above the taskbar, so a large scan result cannot push the menu below the
    /// bottom edge of the display.
    fn update_network_menu_geometry(&mut self, fb_width: u32, fb_height: u32) {
        // Position the menu above the WiFi icon, right-aligned to stay on-screen
        let wifi_icon_x = self.taskbar.wifi_icon_x(fb_width);
        // Right-align the menu with the WiFi icon so it doesn't extend past fb_width
        self.net_menu_x = if wifi_icon_x + network_menu::NET_MENU_WIDTH > fb_width {
            fb_width.saturating_sub(network_menu::NET_MENU_WIDTH)
        } else {
            wifi_icon_x
        };
        let available_height = fb_height.saturating_sub(crate::style::taskbar_height());
        let max_rows = available_height
            .saturating_sub(network_menu::NET_MENU_PADDING + network_menu::NET_MENU_ITEM_HEIGHT)
            / network_menu::NET_MENU_ITEM_HEIGHT;
        self.net_visible_rows = max_rows.max(1) as usize;
        self.net_visible_rows = self.net_visible_rows.min(self.ap_list.len().max(1));
        let max_offset = self.ap_list.len().saturating_sub(self.net_visible_rows);
        self.net_scroll_offset = self.net_scroll_offset.min(max_offset);
        if let Some(selected) = self.net_selected_idx {
            if selected < self.net_scroll_offset {
                self.net_scroll_offset = selected;
            } else if selected >= self.net_scroll_offset + self.net_visible_rows {
                self.net_scroll_offset = selected + 1 - self.net_visible_rows;
            }
            self.net_scroll_offset = self.net_scroll_offset.min(max_offset);
        }
        let menu_h = network_menu::menu_height(self.net_visible_rows);
        self.net_menu_y = fb_height
            .saturating_sub(crate::style::taskbar_height())
            .saturating_sub(menu_h);
    }

    /// Dismiss the network menu.
    pub fn dismiss_network_menu(&mut self) {
        if self.network_menu_open {
            let menu_h = network_menu::menu_height(self.net_visible_rows);
            self.push_dirty_rect(crate::scene::DirtyRect::new(
                self.net_menu_x,
                self.net_menu_y,
                network_menu::NET_MENU_WIDTH,
                menu_h,
            ));
            self.network_menu_open = false;
        }
        self.net_selected_idx = None;
        self.net_scroll_offset = 0;
    }

    /// Close the password dialog and invalidate the area it occupied.
    ///
    /// The dialog is an overlay. Clearing its state without dirtying the old
    /// rectangle leaves its pixels behind until a later repaint covers them.
    pub fn dismiss_password_dialog(&mut self) {
        if self.pwd_dialog_open {
            self.push_dirty_rect(crate::scene::DirtyRect::new(
                self.pwd_dialog_x,
                self.pwd_dialog_y,
                network_menu::PWD_DIALOG_W,
                network_menu::PWD_DIALOG_H,
            ));
        }
        self.pwd_dialog_open = false;
        self.pwd_target_ap = None;
        self.pwd_dialog_password.clear();
        self.pwd_dialog_cursor = 0;
        self.shift_held = false;
    }

    /// Update the access point list for the network menu.
    /// Returns `true` if the list or status actually changed.
    pub fn update_ap_list(&mut self, aps: alloc::vec::Vec<ApDisplay>, status: NetStatus) -> bool {
        let changed = self.ap_list != aps || self.net_status != status;
        if changed {
            // A scan can change both the number of visible rows and the
            // menu's y-position. Invalidate the old rectangle before
            // replacing the list so rows removed by the scan are repainted.
            if self.network_menu_open {
                self.push_dirty_rect(crate::scene::DirtyRect::new(
                    self.net_menu_x,
                    self.net_menu_y,
                    network_menu::NET_MENU_WIDTH,
                    network_menu::menu_height(self.net_visible_rows),
                ));
            }
            self.ap_list = aps;
            self.net_status = status;
            self.net_selected_idx = match (self.net_selected_idx, self.ap_list.len()) {
                (_, 0) => None,
                (Some(index), len) => Some(index.min(len - 1)),
                (None, _) if self.network_menu_open => Some(0),
                (None, _) => None,
            };
            let max_offset = self.ap_list.len().saturating_sub(self.net_visible_rows);
            self.net_scroll_offset = self.net_scroll_offset.min(max_offset);
            self.wifi_networks_visible = match &self.net_status {
                NetStatus::NoDevice => false,
                _ => true,
            };
        }
        changed
    }

    /// Move the highlighted network entry by one or more rows.
    pub fn move_network_selection(&mut self, delta: i32) -> bool {
        if !self.network_menu_open {
            return false;
        }
        let len = self.ap_list.len();
        if len == 0 {
            return true;
        }
        let current = self.net_selected_idx.unwrap_or(0) as i32;
        let next = (current + delta).rem_euclid(len as i32) as usize;
        self.net_selected_idx = Some(next);
        if next < self.net_scroll_offset {
            self.net_scroll_offset = next;
        } else if next >= self.net_scroll_offset + self.net_visible_rows {
            self.net_scroll_offset = next + 1 - self.net_visible_rows;
        }
        let menu_h = network_menu::menu_height(self.net_visible_rows);
        self.push_dirty_rect(crate::scene::DirtyRect::new(
            self.net_menu_x,
            self.net_menu_y,
            network_menu::NET_MENU_WIDTH,
            menu_h,
        ));
        true
    }

    /// Scroll the AP viewport by `delta` rows. Positive values move down.
    pub fn scroll_network_menu(&mut self, delta: i32) -> bool {
        if !self.network_menu_open || self.ap_list.is_empty() {
            return false;
        }
        let max_offset = self.ap_list.len().saturating_sub(self.net_visible_rows);
        let next = (self.net_scroll_offset as i32 + delta).clamp(0, max_offset as i32) as usize;
        if next == self.net_scroll_offset {
            return true;
        }
        self.net_scroll_offset = next;
        if let Some(selected) = self.net_selected_idx {
            self.net_selected_idx = Some(
                selected
                    .max(next)
                    .min(next + self.net_visible_rows.saturating_sub(1)),
            );
        }
        self.push_dirty_rect(crate::scene::DirtyRect::new(
            self.net_menu_x,
            self.net_menu_y,
            network_menu::NET_MENU_WIDTH,
            network_menu::menu_height(self.net_visible_rows),
        ));
        true
    }

    /// Activate the highlighted network entry from keyboard input.
    pub fn activate_network_selection(&mut self, fb_width: u32, fb_height: u32) -> bool {
        if !self.network_menu_open {
            return false;
        }
        let Some(index) = self.net_selected_idx else {
            return true;
        };
        self.activate_network_ap(index, fb_width, fb_height);
        true
    }

    fn activate_network_ap(&mut self, index: usize, fb_width: u32, fb_height: u32) {
        let Some(ap) = self.ap_list.get(index) else {
            return;
        };
        if ap.has_lock {
            self.pwd_dialog_open = true;
            self.pwd_dialog_ssid = ap.ssid.clone();
            self.pwd_dialog_password = String::new();
            self.pwd_dialog_cursor = 0;
            self.shift_held = false;
            self.pwd_target_ap = Some(index);
            self.pwd_dialog_x = (fb_width / 2).saturating_sub(network_menu::PWD_DIALOG_W / 2);
            self.pwd_dialog_y = (fb_height / 2).saturating_sub(network_menu::PWD_DIALOG_H / 2);
        } else {
            self.menu_action_pending = Some(DesktopAction::ConnectAp(index));
        }
        let menu_h = network_menu::menu_height(self.net_visible_rows);
        self.wm.dirty_rects.push(crate::scene::DirtyRect::new(
            self.net_menu_x,
            self.net_menu_y,
            network_menu::NET_MENU_WIDTH,
            menu_h,
        ));
        self.network_menu_open = false;
        self.net_selected_idx = None;
    }

    /// Dismiss the active menu.
    pub fn dismiss_menu(&mut self) {
        if let Some(menu) = self.active_menu.take() {
            self.push_dirty_rect(crate::scene::DirtyRect::new(
                menu.x,
                menu.y,
                menu.width,
                menu.height,
            ));
        }
    }

    /// Check if a point (fb pixel coords) hits a taskbar button.
    ///
    /// Returns the `WindowId` of the taskbar entry whose button
    /// contains the point, or `None`.
    pub fn taskbar_window_at(
        &self,
        px: i32,
        py: i32,
        fb_width: u32,
        fb_height: u32,
    ) -> Option<WindowId> {
        let bar_y = fb_height.saturating_sub(crate::style::taskbar_height()) as i32;
        if py < bar_y {
            return None;
        }
        // Simple linear scan matching the taskbar render layout.
        if crate::style::kind() != crate::common::ShellKind::Basalt {
            for (index, entry) in self.taskbar.entries.iter().enumerate() {
                if let Some((x, y, w, h)) = crate::style::taskbar_entry_rect(
                    index,
                    self.taskbar.entries.len(),
                    fb_width,
                    fb_height,
                ) && px >= x
                    && px < x + w as i32
                    && py >= y
                    && py < y + h as i32
                {
                    return Some(entry.id);
                }
            }
            return None;
        }
        for (index, entry) in self.taskbar.entries.iter().enumerate() {
            let Some((btn_x, btn_y, btn_w, btn_h)) = crate::style::taskbar_entry_rect(
                index,
                self.taskbar.entries.len(),
                fb_width,
                fb_height,
            ) else {
                continue;
            };
            let bx_end = btn_x + btn_w as i32;
            let by_end = btn_y + btn_h as i32;
            if px >= btn_x && px < bx_end && py >= btn_y && py < by_end {
                return Some(entry.id);
            }
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
    /// Returns `true` when any visible state changed (entry count,
    /// WiFi status, or signal strength) that requires a taskbar redraw.
    pub fn update_taskbar(&mut self) -> bool {
        let prev_wifi = self.taskbar.wifi_connected;
        let prev_wifi_visible = self.taskbar.wifi_visible;
        let prev_wifi_signal = self.taskbar.wifi_signal;
        let entries_changed = self.taskbar.update_from_windows(self.wm.windows());
        // Update clock text on taskbar
        self.taskbar.clock_text = self.clock_text.clone();
        // Update WiFi state on taskbar
        self.taskbar.wifi_connected = matches!(&self.net_status, NetStatus::Connected(_, _));
        self.taskbar.wifi_visible = self.wifi_networks_visible;
        self.taskbar.wifi_signal = self.wifi_signal;
        let wifi_changed = self.taskbar.wifi_connected != prev_wifi
            || self.taskbar.wifi_visible != prev_wifi_visible
            || self.taskbar.wifi_signal != prev_wifi_signal;
        entries_changed || wifi_changed
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
            let old_menu_rect = DirtyRect::new(
                self.net_menu_x,
                self.net_menu_y,
                network_menu::NET_MENU_WIDTH,
                network_menu::menu_height(self.net_visible_rows),
            );
            self.update_network_menu_geometry(fb_width, fb_height);
            let menu_h = network_menu::menu_height(self.net_visible_rows);
            let new_menu_rect = DirtyRect::new(
                self.net_menu_x,
                self.net_menu_y,
                network_menu::NET_MENU_WIDTH,
                menu_h,
            );
            if old_menu_rect != new_menu_rect {
                self.dirty_cache.push(old_menu_rect);
            }
            self.dirty_cache.push(DirtyRect::new(
                self.net_menu_x,
                self.net_menu_y,
                network_menu::NET_MENU_WIDTH,
                menu_h,
            ));
            // Also push WiFi icon area as dirty
            let wifi_icon_x = self.taskbar.wifi_icon_x(fb_width);
            self.dirty_cache.push(DirtyRect::new(
                wifi_icon_x,
                fb_height.saturating_sub(crate::style::taskbar_height()),
                network_menu::NET_ICON_WIDTH,
                crate::style::taskbar_height(),
            ));
        }

        // Password dialog overlay
        if self.pwd_dialog_open {
            self.dirty_cache.push(DirtyRect::new(
                self.pwd_dialog_x,
                self.pwd_dialog_y,
                network_menu::PWD_DIALOG_W,
                network_menu::PWD_DIALOG_H,
            ));
        }
    }

    /// Prepare a frame where only an existing window surface changed.
    ///
    /// Returns `false` when another desktop mutation is pending and the
    /// caller must use the normal background-rendering path.
    pub fn prepare_video_frame(
        &mut self,
        window_id: WindowId,
        cursor_dirty: Option<(DirtyRect, DirtyRect)>,
    ) -> bool {
        if self.needs_full_redraw
            || !self.wm.dirty_rects.is_empty()
            || (self.cursor_moved && cursor_dirty.is_none())
            || self.active_menu.is_some()
            || self.network_menu_open
            || self.pwd_dialog_open
        {
            return false;
        }
        let Some(window) = self
            .wm
            .windows()
            .iter()
            .find(|window| window.id == window_id)
        else {
            return false;
        };
        self.dirty_cache.clear();
        let title_height = if window.title.is_some() {
            crate::style::title_bar_height() as i32
        } else {
            0
        };
        let client_x = window.x.max(0) as u32;
        let client_y = window.y.saturating_add(title_height).max(0) as u32;
        self.dirty_cache.push(DirtyRect::new(
            client_x,
            client_y,
            window.width,
            window.height,
        ));
        if let Some((previous, current)) = cursor_dirty {
            self.dirty_cache.push(previous);
            self.dirty_cache.push(current);
            self.cursor_moved = false;
        }
        true
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
            desktop_icons: Some(&self.desktop_icons),
            active_menu: self.active_menu.as_ref(),
            layered: true,
            network_menu_open: self.network_menu_open,
            net_menu_x: self.net_menu_x,
            net_menu_y: self.net_menu_y,
            net_aps: &self.ap_list,
            net_status: &self.net_status,
            net_selected_idx: self.net_selected_idx,
            net_visible_rows: self.net_visible_rows,
            net_scroll_offset: self.net_scroll_offset,
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
    fn network_menu_keyboard_selection_wraps_and_activates() {
        let mut dt = Desktop::new(0x202020);
        dt.update_ap_list(
            alloc::vec![
                ApDisplay {
                    ssid: String::from("first"),
                    signal_bars: 3,
                    has_lock: false,
                    connected: false,
                },
                ApDisplay {
                    ssid: String::from("second"),
                    signal_bars: 2,
                    has_lock: false,
                    connected: false,
                },
            ],
            NetStatus::Disconnected,
        );
        dt.show_network_menu(800, 600);
        assert_eq!(dt.net_selected_idx, Some(0));

        assert!(dt.move_network_selection(-1));
        assert_eq!(dt.net_selected_idx, Some(1));
        assert!(dt.move_network_selection(1));
        assert_eq!(dt.net_selected_idx, Some(0));

        assert!(dt.activate_network_selection(800, 600));
        assert!(!dt.network_menu_open);
        assert_eq!(dt.menu_action_pending, Some(DesktopAction::ConnectAp(0)));
    }

    #[test]
    fn network_menu_bounds_and_scroll_follow_small_scanout() {
        let mut dt = Desktop::new(0x202020);
        let aps = (0..6)
            .map(|index| ApDisplay {
                ssid: alloc::format!("ap-{index}"),
                signal_bars: 2,
                has_lock: false,
                connected: false,
            })
            .collect();
        dt.update_ap_list(aps, NetStatus::Disconnected);
        dt.show_network_menu(800, 120);

        assert!(dt.net_visible_rows < 6);
        assert!(
            dt.net_menu_y + network_menu::menu_height(dt.net_visible_rows)
                <= 120 - crate::style::taskbar_height()
        );

        for _ in 0..5 {
            assert!(dt.move_network_selection(1));
        }
        assert_eq!(dt.net_selected_idx, Some(5));
        assert!(dt.net_scroll_offset > 0);
        assert!(dt.net_scroll_offset + dt.net_visible_rows >= 6);

        assert!(dt.scroll_network_menu(-1));
        assert!(dt.net_scroll_offset < 5);
    }

    #[test]
    fn changing_scan_rows_invalidates_previous_network_menu_rect() {
        let mut dt = Desktop::new(0x202020);
        let aps = (0..6)
            .map(|index| ApDisplay {
                ssid: alloc::format!("ap-{index}"),
                signal_bars: 2,
                has_lock: false,
                connected: false,
            })
            .collect();
        dt.update_ap_list(aps, NetStatus::Disconnected);
        dt.show_network_menu(800, 120);
        dt.prepare_frame(800, 120);

        let old_rect = crate::scene::DirtyRect::new(
            dt.net_menu_x,
            dt.net_menu_y,
            network_menu::NET_MENU_WIDTH,
            network_menu::menu_height(dt.net_visible_rows),
        );
        dt.update_ap_list(
            alloc::vec![ApDisplay {
                ssid: String::from("remaining"),
                signal_bars: 1,
                has_lock: false,
                connected: false,
            }],
            NetStatus::Disconnected,
        );

        assert!(dt.wm.dirty_rects.contains(&old_rect));
    }

    #[test]
    fn dismissing_password_dialog_invalidates_its_old_rectangle() {
        let mut dt = Desktop::new(0x202020);
        dt.pwd_dialog_open = true;
        dt.pwd_dialog_x = 40;
        dt.pwd_dialog_y = 50;
        dt.dismiss_password_dialog();

        assert!(!dt.pwd_dialog_open);
        assert!(dt.wm.dirty_rects.iter().any(|rect| {
            rect.x == 40
                && rect.y == 50
                && rect.width == network_menu::PWD_DIALOG_W
                && rect.height == network_menu::PWD_DIALOG_H
        }));
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
    fn scene_delegates_cursor_rendering_to_compositor() {
        let dt = Desktop::new(0x202020);
        assert!(dt.scene().cursor.is_some());
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
    fn closing_a_titled_window_reports_its_id_after_removal() {
        let mut dt = Desktop::new(0x202020);
        let id = dt
            .wm
            .create_titled_window(10, 10, 100, 100, 0xFF0000, "Test");
        let window = dt
            .wm
            .windows()
            .iter()
            .find(|window| window.id == id)
            .unwrap();
        dt.set_cursor(
            crate::style::title_button_x(window.x, window.width, 0) + 2,
            window.y + 5,
        );

        assert_eq!(dt.mouse_down(1024, 768), Some(id));
        assert!(dt.wm.windows().iter().all(|window| window.id != id));
    }

    #[test]
    fn moving_titled_window_repaints_its_shadow() {
        let window = crate::window::Window::new_with_title(
            crate::window::WindowId(1),
            20,
            20,
            80,
            60,
            0x224466,
            "Test",
        );
        let old_rect = crate::wm::window_dirty_rect(&window);
        let old_windows = [window];
        let mut incremental = TestTarget::new(180, 140);
        let old_scene = Scene::new(&old_windows, None, 0x101010);
        Compositor::render(&old_scene, &mut incremental);

        let mut window = old_windows.into_iter().next().unwrap();
        window.x = 90;
        window.y = 55;
        let new_rect = crate::wm::window_dirty_rect(&window);
        let windows = [window];
        let dirty = [old_rect, new_rect];
        let dirty_scene = Scene::with_dirty_rects(&windows, None, 0x101010, &dirty);
        Compositor::render(&dirty_scene, &mut incremental);

        let full_scene = Scene::new(&windows, None, 0x101010);
        let mut expected = TestTarget::new(180, 140);
        Compositor::render(&full_scene, &mut expected);
        let mismatch = incremental
            .pixels
            .iter()
            .zip(&expected.pixels)
            .enumerate()
            .find(|(_, (actual, expected))| actual != expected);
        assert_eq!(mismatch, None);
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
    fn power_button_opens_restart_and_shutdown_menu() {
        let mut dt = Desktop::new(0x202020);
        dt.taskbar.clock_text = String::from("2026 0805 1210");
        let (fb_width, fb_height) = (1024, 768);
        let power_x = dt.taskbar.power_icon_x(fb_width);
        dt.set_cursor(
            power_x as i32 + 4,
            fb_height as i32 - crate::style::taskbar_height() as i32 + 4,
        );
        dt.mouse_down(fb_width, fb_height);
        assert_eq!(dt.menu_action_pending, Some(DesktopAction::ShowPowerMenu));

        dt.menu_action_pending = None;
        dt.show_power_menu(fb_width, fb_height);
        let menu = dt.active_menu.as_ref().unwrap();
        assert_eq!(menu.items.len(), 2);
        assert_eq!(menu.items[0].action, "reboot");
        assert_eq!(menu.items[1].action, "shutdown");
        assert!(menu.x + menu.width <= fb_width);
        assert!(menu.y + menu.height <= fb_height - crate::style::taskbar_height());
    }

    #[test]
    fn power_menu_is_clipped_above_taskbar_on_short_framebuffer() {
        let mut dt = Desktop::new(0x202020);
        let (fb_width, fb_height) = (
            160,
            crate::style::taskbar_height() + ITEM_HEIGHT + MENU_BORDER * 2,
        );
        dt.show_power_menu(fb_width, fb_height);
        let menu = dt.active_menu.as_ref().unwrap();
        let usable_height = fb_height - crate::style::taskbar_height();
        assert!(menu.x + menu.width <= fb_width);
        assert!(menu.y + menu.height <= usable_height);
        assert!(menu.items.len() <= 1);
    }

    #[test]
    fn power_menu_is_hidden_when_framebuffer_is_narrower_than_borders() {
        let mut dt = Desktop::new(0x202020);
        dt.show_power_menu(MENU_BORDER, 100);
        let menu = dt.active_menu.as_ref().unwrap();
        assert!(!menu.visible);
        assert!(menu.items.is_empty());
        assert!(menu.to_overlays().is_empty());
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

    #[test]
    fn context_menu_is_clamped_to_the_scanout() {
        let mut dt = Desktop::new(0x202020);
        dt.show_context_menu_in_bounds(999, 999, 320, 400);
        let menu = dt.active_menu.as_ref().unwrap();
        assert!(menu.x + menu.width <= 320);
        assert!(menu.y + menu.height <= 400);
    }
}
