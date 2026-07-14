//! Taskbar — a thin bar at the bottom of the screen.
//!
//! Renders a horizontal bar with:
//! - Background fill
//! - Clock display (system tick converted to "HH:MM:SS")
//! - Window title buttons for each open window
//! - WiFi network indicator icon
//!
//! The taskbar is drawn as an overlay on the compositor output.

/// Taskbar height in pixels.
pub const TASKBAR_HEIGHT: u32 = 28;

/// Taskbar background colour.
pub const TASKBAR_BG: u32 = 0x0F0F1A;

/// Taskbar button / text colour.
pub const TASKBAR_TEXT: u32 = 0xCCCCCC;

/// Taskbar button for focused window.
pub const TASKBAR_ACTIVE_BG: u32 = 0x3A7BD5;

/// Taskbar button for unfocused window.
pub const TASKBAR_INACTIVE_BG: u32 = 0x333344;

/// A single taskbar entry (represents a window).
#[derive(Debug, Clone)]
pub struct TaskbarEntry {
    /// Window ID for click-to-restore / click-to-focus.
    pub id: crate::window::WindowId,
    /// Window title (truncated to fit button).
    pub title: alloc::string::String,
    /// Whether the window has focus.
    pub focused: bool,
}

/// Taskbar state (owned by Desktop / Solvent).
#[derive(Debug)]
pub struct Taskbar {
    /// Current list of task entries.
    pub entries: alloc::vec::Vec<TaskbarEntry>,
    /// Clock text (updated externally).
    pub clock_text: alloc::string::String,
    /// Whether WiFi is connected (for icon display).
    pub wifi_connected: bool,
    /// Is there any WiFi network visible.
    pub wifi_visible: bool,
    /// Signal level 0-100.
    pub wifi_signal: u8,
    /// Live debug status messages from drivers (source, message).
    /// Displayed to the left of the WiFi icon, newest last.
    pub debug_msgs: alloc::vec::Vec<(alloc::string::String, alloc::string::String)>,
}

impl Taskbar {
    pub fn new() -> Self {
        Self {
            entries: alloc::vec::Vec::new(),
            clock_text: alloc::string::String::new(),
            wifi_connected: false,
            wifi_visible: false,
            wifi_signal: 0,
            debug_msgs: alloc::vec::Vec::new(),
        }
    }

    /// Compute the WiFi icon X position based on clock text width.
    pub fn wifi_icon_x(&self, fb_width: u32) -> u32 {
        let clock_w = if !self.clock_text.is_empty() {
            (self.clock_text.len() as u32 * 8) + 8
        } else {
            0
        };
        fb_width.saturating_sub(clock_w + crate::network_menu::NET_ICON_WIDTH + 8)
    }

    /// Update entries from window list.
    pub fn update_from_windows(&mut self, windows: &[crate::window::Window]) {
        self.entries.clear();
        for w in windows.iter().rev() {
            let title = w
                .title
                .as_ref()
                .map(|t| t.clone())
                .unwrap_or_else(|| alloc::string::String::from("Window"));
            self.entries.push(TaskbarEntry {
                id: w.id,
                title,
                focused: w.focused,
            });
        }
    }

    /// Render the taskbar onto a surface (intended to overlay the framebuffer).
    ///
    /// The surface should be the full framebuffer dimensions; the taskbar
    /// is drawn at the bottom.
    pub fn render(&self, fb: &mut [u32], fb_width: u32, fb_height: u32) {
        let colors = crate::theme::current_colors();
        let bar_y = fb_height.saturating_sub(TASKBAR_HEIGHT);
        let fb_w = fb_width as usize;
        let mut painter = crate::painter::Painter::new(fb, fb_width, fb_height);

        // Fill bar background
        for row in 0..TASKBAR_HEIGHT {
            let y = bar_y + row;
            if y >= fb_height {
                break;
            }
            let row_start = (y as usize) * fb_w;
            painter.fb[row_start..row_start + fb_w].fill(colors.taskbar_bg);
        }

        // WiFi icon X position (used both for icon and as right bound for debug text)
        let wifi_icon_x = self.wifi_icon_x(fb_width);

        // Draw WiFi indicator icon (right side, before clock)
        crate::network_menu::render_wifi_icon(
            painter.fb, fb_width, fb_height,
            wifi_icon_x, bar_y + 6,
            self.wifi_connected,
            self.wifi_visible,
            self.wifi_signal,
        );

        // Draw window buttons (from left)
        let mut btn_x = 4i32;
        let btn_w = 120u32;
        let btn_h = TASKBAR_HEIGHT - 6;
        let btn_y = bar_y + 3;

        for entry in &self.entries {
            if btn_x as u32 + btn_w > fb_width {
                break;
            }
            let bg = if entry.focused {
                colors.taskbar_active_bg
            } else {
                colors.taskbar_inactive_bg
            };
            // Button background
            for row in 0..btn_h {
                let y = btn_y + row;
                let rs = (y as usize) * fb_w + btn_x as usize;
                painter.fb[rs..rs + btn_w as usize].fill(bg);
            }
            // Button text
            let label = if entry.title.len() > 14 {
                &entry.title[..14]
            } else {
                &entry.title
            };
            painter.draw_text(btn_x + 4, btn_y as i32 + 3, label, colors.taskbar_text, 13.0);
            btn_x += btn_w as i32 + 4;
        }

        // Draw debug status messages (between window buttons and WiFi icon)
        if !self.debug_msgs.is_empty() {
            let (last_source, last_msg) = &self.debug_msgs[self.debug_msgs.len() - 1];
            let debug_text = if last_source.is_empty() {
                alloc::format!("[{}]", last_msg)
            } else {
                alloc::format!("{}: {}", last_source, last_msg)
            };
            let dx = btn_x + 4;
            let dy = (btn_y + 3) as i32;
            painter.draw_text(dx, dy, &debug_text, colors.taskbar_text, 13.0);
        }

        // Draw clock on the right
        if !self.clock_text.is_empty() {
            let clock_y = (btn_y + 3) as i32;
            let clock_x = (fb_width as i32).saturating_sub(100);
            painter.draw_text(clock_x, clock_y, &self.clock_text, colors.taskbar_text, 13.0);
        }
    }
}
