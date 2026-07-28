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
        let clock_w = if !self.clock_text.is_empty() {
            (self.clock_text.len() as u32 * 8) + 8
        } else {
            0
        };
        fb_width.saturating_sub(clock_w + crate::network_menu::NET_ICON_WIDTH + 8)
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
