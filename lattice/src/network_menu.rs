//! Network/WiFi menu UI components.
//!
//! Provides the access point list menu, password input dialog,
//! and WiFi status indicator for the taskbar.

use crate::painter::Painter;
use alloc::string::String;

// ── Constants ──────────────────────────────────────────────────

/// Network icon area width in the taskbar.
pub const NET_ICON_WIDTH: u32 = 32;
/// Network icon area height.
pub const NET_ICON_HEIGHT: u32 = 28;

/// Colors for the WiFi icon.
const WIFI_ACTIVE: u32 = 0x4A90D9;
const WIFI_INACTIVE: u32 = 0x666666;
const WIFI_CONNECTED: u32 = 0x33CC33;

/// Menu dimensions for network AP list.
pub const NET_MENU_WIDTH: u32 = 220;
pub const NET_MENU_ITEM_HEIGHT: u32 = 28;
pub const NET_MENU_BORDER: u32 = 1;
pub const NET_MENU_BG: u32 = 0x2A2A3E;
pub const NET_MENU_TEXT: u32 = 0xCCCCCC;
pub const NET_MENU_BORDER_COLOR: u32 = 0x4A90D9;
pub const NET_MENU_HOVER: u32 = 0x3A7BD5;
pub const NET_MENU_LOCKED: u32 = 0xE6A817;
pub const NET_MENU_SIGNAL_COLOR: u32 = 0x4A90D9;
pub const NET_MENU_DISCONNECTED: u32 = 0x999999;

/// Password dialog constants.
pub const PWD_DIALOG_W: u32 = 320;
pub const PWD_DIALOG_H: u32 = 140;
const PWD_INPUT_W: u32 = 280;
const PWD_INPUT_H: u32 = 24;
const PWD_BG: u32 = 0x2A2A3E;
const PWD_BORDER_WIDTH: u32 = 1;
const PWD_BORDER_COLOR: u32 = 0x4A90D9;
const PWD_TEXT: u32 = 0xCCCCCC;
const PWD_INPUT_BG: u32 = 0x1A1A2E;
const PWD_INPUT_TEXT: u32 = 0xFFFFFF;

// ── Scan result for display ──────────────────────────────────

/// Display-friendly access point info.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApDisplay {
    pub ssid: String,
    pub signal_bars: u8,
    pub has_lock: bool,
    pub connected: bool,
}

// ── Network status ────────────────────────────────────────────

/// Overall network status for menu display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetStatus {
    NoDevice,
    Scanning,
    Disconnected,
    Connecting(String),
    Connected(String, String), // SSID, IP
    Error(String),
}

// ── WiFi icon rendering ───────────────────────────────────────

/// Draw the WiFi signal indicator icon into the framebuffer.
///
/// The icon is a 3-bar signal strength indicator drawn at (x, y)
/// with the given connection state.
pub fn render_wifi_icon(
    fb: &mut [u32],
    fb_width: u32,
    fb_height: u32,
    x: u32,
    y: u32,
    connected: bool,
    has_networks: bool,
    signal_level: u8,
) {
    let color = if connected {
        WIFI_CONNECTED
    } else if has_networks {
        WIFI_ACTIVE
    } else {
        WIFI_INACTIVE
    };

    let fw = fb_width as usize;
    let bar_y = y;

    // Draw 3 signal bars of increasing height
    let bar_width: u32 = 4;
    let bar_spacing: u32 = 2;
    let max_height: u32 = 16;

    for i in 0..3u32 {
        let bx = x + i * (bar_width + bar_spacing);
        let bh = 4 + i * 4; // heights: 4, 8, 12
        let by = bar_y + (max_height - bh);

        let bar_color = if (signal_level as u32) > i * 40 {
            color
        } else {
            WIFI_INACTIVE
        };

        for row in 0..bh {
            let py = by + row;
            if py >= fb_height {
                continue;
            }
            for col in 0..bar_width {
                let px = bx + col;
                if px >= fb_width {
                    continue;
                }
                let idx = (py as usize) * fw + px as usize;
                if idx < fb.len() {
                    fb[idx] = bar_color;
                }
            }
        }
    }

    // Draw small circle at bottom
    if connected {
        for row in 0..3u32 {
            let py = bar_y + max_height + 1 + row;
            if py >= fb_height {
                continue;
            }
            for col in 0..5u32 {
                let px = x + 3 + col;
                if px >= fb_width {
                    continue;
                }
                let idx = (py as usize) * fw + px as usize;
                if idx < fb.len() {
                    fb[idx] = WIFI_CONNECTED;
                }
            }
        }
    }
}

/// Render the password dialog for connecting to an AP.
///
/// This renders a window-like dialog with:
/// - Title bar showing the SSID
/// - Password input field
/// - Connect and Cancel buttons
pub fn render_password_dialog(
    fb: &mut [u32],
    fb_width: u32,
    fb_height: u32,
    dialog_x: u32,
    dialog_y: u32,
    ssid: &str,
    password: &str,
    cursor_pos: usize,
) {
    let fw = fb_width as usize;

    // Dialog background
    for row in 0..PWD_DIALOG_H {
        let py = dialog_y + row;
        if py >= fb_height {
            continue;
        }
        for col in 0..PWD_DIALOG_W {
            let px = dialog_x + col;
            if px >= fb_width {
                continue;
            }
            let idx = (py as usize) * fw + px as usize;
            if idx < fb.len() {
                // Border
                if row < PWD_BORDER_WIDTH
                    || row >= PWD_DIALOG_H - PWD_BORDER_WIDTH
                    || col < PWD_BORDER_WIDTH
                    || col >= PWD_DIALOG_W - PWD_BORDER_WIDTH
                {
                    fb[idx] = PWD_BORDER_COLOR;
                } else {
                    fb[idx] = PWD_BG;
                }
            }
        }
    }

    // Title
    let title_x = dialog_x + 10;
    let title_y = dialog_y + 10;
    render_menu_text(
        fb,
        fb_width,
        fb_height,
        title_x,
        title_y,
        "Connect to ",
        PWD_TEXT,
    );
    render_menu_text(
        fb,
        fb_width,
        fb_height,
        title_x + 11 * 8,
        title_y,
        ssid,
        PWD_TEXT,
    );

    // Password input field
    let input_x = dialog_x + (PWD_DIALOG_W - PWD_INPUT_W) / 2;
    let input_y = dialog_y + 40;

    for row in 0..PWD_INPUT_H {
        let py = input_y + row;
        if py >= fb_height {
            continue;
        }
        for col in 0..PWD_INPUT_W {
            let px = input_x + col;
            if px >= fb_width {
                continue;
            }
            let idx = (py as usize) * fw + px as usize;
            if idx < fb.len() {
                if row == 0 || row == PWD_INPUT_H - 1 || col == 0 || col == PWD_INPUT_W - 1 {
                    fb[idx] = PWD_BORDER_COLOR;
                } else {
                    fb[idx] = PWD_INPUT_BG;
                }
            }
        }
    }

    // Draw password text (masked with *)
    let text_x = (input_x + 4) as i32;
    let text_y = (input_y + 6) as i32;
    let masked = "*".repeat(password.len());
    let mut p = Painter::new(fb, fb_width, fb_height);
    p.draw_text(text_x, text_y, &masked, PWD_INPUT_TEXT, 13.0);

    // Cursor blink - draw a short vertical bar
    let cursor_x = text_x + (cursor_pos as i32) * 8;
    if (cursor_x as u32) < input_x + PWD_INPUT_W - 4 && cursor_x < fb_width as i32 {
        // Draw a vertical bar (5 pixels tall)
        for dy in 0..5 {
            let cursor_y = text_y + 8 + dy;
            if cursor_y >= 0 && (cursor_y as u32) < fb_height {
                let idx = (cursor_y as usize) * fw + cursor_x as usize;
                if idx < fb.len() {
                    fb[idx] = PWD_INPUT_TEXT;
                }
            }
        }
    }

    // Connect button
    let btn_w = 80;
    let btn_h = 24;
    let btn_x = dialog_x + PWD_DIALOG_W - btn_w - 20;
    let btn_y = dialog_y + PWD_DIALOG_H - btn_h - 10;
    render_button(
        fb, fb_width, fb_height, btn_x, btn_y, btn_w, btn_h, "Connect", false,
    );

    // Cancel button
    let cancel_x = btn_x - btn_w - 10;
    render_button(
        fb, fb_width, fb_height, cancel_x, btn_y, btn_w, btn_h, "Cancel", false,
    );
}

/// Render a simple button.
fn render_button(
    fb: &mut [u32],
    fb_width: u32,
    fb_height: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    label: &str,
    _hover: bool,
) {
    let fw = fb_width as usize;
    let btn_bg = 0x3A7BD5;
    let btn_border = 0x4A90D9;

    for row in 0..h {
        let py = y + row;
        if py >= fb_height {
            continue;
        }
        for col in 0..w {
            let px = x + col;
            if px >= fb_width {
                continue;
            }
            let idx = (py as usize) * fw + px as usize;
            if idx < fb.len() {
                if row < 1 || row >= h - 1 || col < 1 || col >= w - 1 {
                    fb[idx] = btn_border;
                } else {
                    fb[idx] = btn_bg;
                }
            }
        }
    }

    // Label using Painter TTF
    let text_x = (x as i32) + (w as i32 - (label.len() as i32) * 8) / 2;
    let text_y = (y + (h - 12) / 2) as i32;
    let mut p = Painter::new(fb, fb_width, fb_height);
    p.draw_text(text_x, text_y, label, 0xFFFFFF, 13.0);
}

/// Render the network menu (list of APs).
pub fn render_network_menu(
    fb: &mut [u32],
    fb_width: u32,
    fb_height: u32,
    menu_x: u32,
    menu_y: u32,
    aps: &[ApDisplay],
    status: &NetStatus,
    _hover_idx: Option<usize>,
) {
    let fw = fb_width as usize;

    // Calculate menu height
    let item_count = aps.len() + 1; // +1 for status/title line
    let menu_h = 4 + item_count as u32 * NET_MENU_ITEM_HEIGHT;

    // Menu background
    for row in 0..menu_h {
        let py = menu_y + row;
        if py >= fb_height {
            continue;
        }
        for col in 0..NET_MENU_WIDTH {
            let px = menu_x + col;
            if px >= fb_width {
                continue;
            }
            let idx = (py as usize) * fw + px as usize;
            if idx < fb.len() {
                if row < NET_MENU_BORDER
                    || row >= menu_h - NET_MENU_BORDER
                    || col < NET_MENU_BORDER
                    || col >= NET_MENU_WIDTH - NET_MENU_BORDER
                {
                    fb[idx] = NET_MENU_BORDER_COLOR;
                } else {
                    fb[idx] = NET_MENU_BG;
                }
            }
        }
    }

    // Status line
    match status {
        NetStatus::NoDevice => {
            render_menu_text(
                fb,
                fb_width,
                fb_height,
                menu_x + 6,
                menu_y + 4,
                "No WiFi device",
                NET_MENU_TEXT,
            );
        }
        NetStatus::Scanning => {
            render_menu_text(
                fb,
                fb_width,
                fb_height,
                menu_x + 6,
                menu_y + 4,
                "Scanning...",
                NET_MENU_TEXT,
            );
        }
        NetStatus::Disconnected => {
            render_menu_text(
                fb,
                fb_width,
                fb_height,
                menu_x + 6,
                menu_y + 4,
                "Select a network:",
                NET_MENU_TEXT,
            );
        }
        NetStatus::Connecting(ssid) => {
            let mut cur_x = menu_x + 6;
            render_menu_text(
                fb,
                fb_width,
                fb_height,
                cur_x,
                menu_y + 4,
                "Connecting to ",
                NET_MENU_TEXT,
            );
            cur_x += 14 * 8;
            render_menu_text(
                fb,
                fb_width,
                fb_height,
                cur_x,
                menu_y + 4,
                ssid,
                NET_MENU_TEXT,
            );
            cur_x += (ssid.len() as u32) * 8;
            render_menu_text(
                fb,
                fb_width,
                fb_height,
                cur_x,
                menu_y + 4,
                "...",
                NET_MENU_TEXT,
            );
        }
        NetStatus::Connected(ssid, ip) => {
            let mut cur_x = menu_x + 6;
            render_menu_text(
                fb,
                fb_width,
                fb_height,
                cur_x,
                menu_y + 4,
                "Connected to ",
                NET_MENU_TEXT,
            );
            cur_x += 13 * 8;
            render_menu_text(
                fb,
                fb_width,
                fb_height,
                cur_x,
                menu_y + 4,
                ssid,
                NET_MENU_TEXT,
            );
            cur_x += (ssid.len() as u32) * 8;
            render_menu_text(
                fb,
                fb_width,
                fb_height,
                cur_x,
                menu_y + 4,
                " (",
                NET_MENU_TEXT,
            );
            cur_x += 2 * 8;
            render_menu_text(
                fb,
                fb_width,
                fb_height,
                cur_x,
                menu_y + 4,
                ip,
                NET_MENU_TEXT,
            );
            cur_x += (ip.len() as u32) * 8;
            render_menu_text(
                fb,
                fb_width,
                fb_height,
                cur_x,
                menu_y + 4,
                ")",
                NET_MENU_TEXT,
            );
        }
        NetStatus::Error(e) => {
            let mut cur_x = menu_x + 6;
            render_menu_text(
                fb,
                fb_width,
                fb_height,
                cur_x,
                menu_y + 4,
                "Error: ",
                NET_MENU_TEXT,
            );
            cur_x += 7 * 8;
            render_menu_text(fb, fb_width, fb_height, cur_x, menu_y + 4, e, NET_MENU_TEXT);
        }
    }

    // Draw each AP
    for (i, ap) in aps.iter().enumerate() {
        let item_y = menu_y + NET_MENU_ITEM_HEIGHT + (i as u32) * NET_MENU_ITEM_HEIGHT;

        // Skip if off screen
        if item_y + NET_MENU_ITEM_HEIGHT > fb_height {
            break;
        }

        // Signal bars
        let signal_x = menu_x + 6;
        let signal_y = item_y + 10;
        let bars = (ap.signal_bars.min(3)) as u32;
        for b in 0..bars {
            let bx = signal_x + b * 6;
            let bh = 4 + b * 3;
            let by = signal_y + 10 - bh;
            for row in 0..bh {
                let py = by + row;
                if py >= fb_height {
                    continue;
                }
                let idx = (py as usize) * fw + bx as usize;
                for col in 0..3 {
                    let dx = idx + col;
                    if dx < fb.len() {
                        fb[dx] = if ap.connected {
                            WIFI_CONNECTED
                        } else {
                            NET_MENU_SIGNAL_COLOR
                        };
                    }
                }
            }
        }

        // Lock icon for secured networks
        if ap.has_lock {
            let lock_x = signal_x + 22;
            let lock_y = item_y + 6;
            // Draw simple lock shape
            for col in 0..5 {
                for row in 0..4 {
                    let px = lock_x + col;
                    let py = lock_y + row;
                    if px < fb_width && py < fb_height {
                        let idx = (py as usize) * fw + px as usize;
                        if idx < fb.len() {
                            let is_lock_top = row == 0 && col > 0 && col < 4;
                            let is_lock_body = row >= 1;
                            if is_lock_top || is_lock_body {
                                fb[idx] = NET_MENU_LOCKED;
                            }
                        }
                    }
                }
            }
        }

        // SSID text
        let text_x = signal_x + 30;
        render_menu_text(
            fb,
            fb_width,
            fb_height,
            text_x,
            item_y + 8,
            &ap.ssid,
            if ap.connected {
                WIFI_CONNECTED
            } else {
                NET_MENU_TEXT
            },
        );

        // Connected indicator
        if ap.connected {
            let check_x = menu_x + NET_MENU_WIDTH - 20;
            let check_text = "\u{2713}"; // checkmark
            render_menu_text(
                fb,
                fb_width,
                fb_height,
                check_x,
                item_y + 8,
                check_text,
                WIFI_CONNECTED,
            );
        }
    }
}

/// Helper to render menu text using Painter TTF.
fn render_menu_text(
    fb: &mut [u32],
    fb_width: u32,
    fb_height: u32,
    x: u32,
    y: u32,
    text: &str,
    color: u32,
) {
    let mut p = Painter::new(fb, fb_width, fb_height);
    p.draw_text(x as i32, y as i32, text, color, 13.0);
}

/// Check if a point is within the WiFi icon area in the taskbar.
pub fn hit_wifi_icon(px: i32, py: i32, _fb_width: u32, fb_height: u32, icon_x: u32) -> bool {
    let bar_y = fb_height.saturating_sub(crate::taskbar::TASKBAR_HEIGHT) as i32;
    if py < bar_y {
        return false;
    }
    let ix = icon_x as i32;
    px >= ix && px < ix + NET_ICON_WIDTH as i32
}

/// Check if a point hits an AP entry in the network menu.
pub fn hit_ap_entry(px: i32, py: i32, menu_x: u32, menu_y: u32, num_aps: usize) -> Option<usize> {
    let start_y = menu_y + NET_MENU_ITEM_HEIGHT; // After status line
    let end_y = start_y + (num_aps as u32) * NET_MENU_ITEM_HEIGHT;

    let px_u = px as u32;
    let py_u = py as u32;

    if px_u < menu_x || px_u >= menu_x + NET_MENU_WIDTH {
        return None;
    }
    if py_u < start_y || py_u >= end_y {
        return None;
    }

    let rel_y = py_u - start_y;
    let idx = (rel_y / NET_MENU_ITEM_HEIGHT) as usize;
    if idx < num_aps { Some(idx) } else { None }
}

/// Check if a point hits the network menu area (for dismissal).
pub fn hit_network_menu(px: i32, py: i32, menu_x: u32, menu_y: u32, num_aps: usize) -> bool {
    let menu_h = 4 + (num_aps + 1) as u32 * NET_MENU_ITEM_HEIGHT;
    let px_u = px as u32;
    let py_u = py as u32;
    px_u >= menu_x && px_u < menu_x + NET_MENU_WIDTH && py_u >= menu_y && py_u < menu_y + menu_h
}
