//! Taskbar — a thin bar at the bottom of the screen.
//!
//! Renders a horizontal bar with:
//! - Background fill
//! - Clock display (system tick converted to "HH:MM:SS")
//! - Window title buttons for each open window
//!
//! The taskbar is drawn as an overlay on the compositor output.

use crate::surface::Surface;

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
}

impl Taskbar {
    pub fn new() -> Self {
        Self {
            entries: alloc::vec::Vec::new(),
            clock_text: alloc::string::String::new(),
        }
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
        let bar_y = fb_height.saturating_sub(TASKBAR_HEIGHT);
        let fb_w = fb_width as usize;

        // Fill bar background
        for row in 0..TASKBAR_HEIGHT {
            let y = bar_y + row;
            if y >= fb_height {
                break;
            }
            let row_start = (y as usize) * fb_w;
            fb[row_start..row_start + fb_w].fill(TASKBAR_BG);
        }

        // Draw window buttons (from left)
        let mut btn_x = 4u32;
        let btn_w = 120u32;
        let btn_h = TASKBAR_HEIGHT - 6;
        let btn_y = bar_y + 3;

        for entry in &self.entries {
            if btn_x + btn_w > fb_width {
                break;
            }
            let bg = if entry.focused {
                TASKBAR_ACTIVE_BG
            } else {
                TASKBAR_INACTIVE_BG
            };
            // Button background
            for row in 0..btn_h {
                let y = btn_y + row;
                let rs = (y as usize) * fb_w + btn_x as usize;
                fb[rs..rs + btn_w as usize].fill(bg);
            }
            // Button text
            let label = if entry.title.len() > 14 {
                &entry.title[..14]
            } else {
                &entry.title
            };
            let tx = btn_x + 4;
            let ty = btn_y + 3;
            for (i, ch) in label.bytes().enumerate() {
                if ch < 32 || ch > 126 {
                    continue;
                }
                for row in 0..12 {
                    let y = ty + row;
                    if y >= fb_height {
                        continue;
                    }
                    for col in 0..8 {
                        let x = tx + (i as u32) * 8 + col;
                        if x >= fb_width {
                            continue;
                        }
                        if crate::font::get_glyph_pixel(ch, row, col) {
                            fb[(y as usize) * fb_w + x as usize] = TASKBAR_TEXT;
                        }
                    }
                }
            }
            btn_x += btn_w + 4;
        }

        // Draw clock on the right
        if !self.clock_text.is_empty() {
            let clock_len = self.clock_text.len() as u32 * 8;
            let clock_x = fb_width.saturating_sub(clock_len + 8);
            let clock_y = btn_y + 3;
            for (i, ch) in self.clock_text.bytes().enumerate() {
                if ch < 32 || ch > 126 {
                    continue;
                }
                for row in 0..12 {
                    for col in 0..8 {
                        let x = clock_x + (i as u32) * 8 + col;
                        let y = clock_y + row;
                        if x < fb_width && y < fb_height {
                            if crate::font::get_glyph_pixel(ch, row, col) {
                                fb[(y as usize) * fb_w + x as usize] = TASKBAR_TEXT;
                            }
                        }
                    }
                }
            }
        }
    }
}
