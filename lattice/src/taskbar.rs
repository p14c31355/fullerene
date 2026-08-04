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

/// Active shell taskbar height.  `TASKBAR_HEIGHT` remains as a compatibility
/// constant for external callers that only support Basalt.
#[inline]
pub fn height() -> u32 {
    crate::style::taskbar_height()
}

/// Taskbar background colour.
pub const TASKBAR_BG: u32 = 0x0F0F1A;

/// Taskbar button / text colour.
pub const TASKBAR_TEXT: u32 = 0xCCCCCC;

/// Taskbar button for focused window.
pub const TASKBAR_ACTIVE_BG: u32 = 0x3A7BD5;

/// Taskbar button for unfocused window.
pub const TASKBAR_INACTIVE_BG: u32 = 0x333344;

/// Width reserved for the WiFi status icon.
pub const WIFI_STATUS_WIDTH: u32 = crate::network_menu::NET_ICON_WIDTH;
/// Width reserved for the power status icon.
pub const POWER_STATUS_WIDTH: u32 = 32;
/// Gap between status icons and the clock.
pub const STATUS_GAP: u32 = 8;

/// A single taskbar entry (represents a window).
#[derive(Debug, Clone, PartialEq, Eq)]
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
        self.clock_x(fb_width)
            .saturating_sub(STATUS_GAP + POWER_STATUS_WIDTH + STATUS_GAP + WIFI_STATUS_WIDTH)
    }

    /// Compute the left edge of the clock text in the status area.
    pub fn clock_x(&self, fb_width: u32) -> u32 {
        fb_width.saturating_sub(self.clock_text.len() as u32 * 8)
    }

    /// Compute the power icon X position, between WiFi and the clock.
    pub fn power_icon_x(&self, fb_width: u32) -> u32 {
        self.clock_x(fb_width)
            .saturating_sub(STATUS_GAP + POWER_STATUS_WIDTH)
    }

    /// Left edge of the rounded right-hand status group used by modern shells.
    pub fn status_group_x(&self, fb_width: u32) -> u32 {
        self.wifi_icon_x(fb_width).saturating_sub(STATUS_GAP)
    }

    /// Update entries from window list.
    pub fn update_from_windows(&mut self, windows: &[crate::window::Window]) -> bool {
        let changed = self.entries.len() != windows.len()
            || self
                .entries
                .iter()
                .zip(windows.iter().rev())
                .any(|(entry, window)| {
                    let title = window.title.as_deref().unwrap_or("Window");
                    entry.id != window.id || entry.title != title || entry.focused != window.focused
                });
        if !changed {
            return false;
        }
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
        true
    }

    /// Render the taskbar onto a surface (intended to overlay the framebuffer).
    ///
    /// The surface should be the full framebuffer dimensions; the taskbar
    /// is drawn at the bottom.
    pub fn render(&self, fb: &mut [u32], fb_width: u32, fb_height: u32) {
        let mut painter = crate::painter::Painter::new(fb, fb_width, fb_height);
        crate::style::style_for(crate::style::variant()).draw_taskbar(&mut painter, self);
    }
}

/// Draw a compact, platform-neutral power glyph.
pub fn render_power_icon(canvas: &mut crate::painter::Painter<'_>, x: u32, y: u32, color: u32) {
    // A 20×20 pixel power glyph: a vertical stem and a deliberately
    // lightweight broken ring, so it remains legible in all three shells.
    canvas.fill_rect(x as i32 + 9, y as i32, 2, 9, color);
    for (dx, dy) in [
        (5, 3),
        (4, 4),
        (3, 5),
        (2, 7),
        (2, 10),
        (3, 12),
        (5, 14),
        (7, 15),
        (12, 15),
        (14, 14),
        (16, 12),
        (17, 10),
        (17, 7),
        (16, 5),
        (15, 4),
        (14, 3),
    ] {
        canvas.set_pixel(x + dx, y + dy, color);
        canvas.set_pixel(x + dx + 1, y + dy, color);
    }
}
