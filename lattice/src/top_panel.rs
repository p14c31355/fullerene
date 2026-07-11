//! Top panel — GNOME-style upper bar for Fullerene OS.
//!
//! Renders a thin bar at the top of the screen with:
//! - "Activities" button (left) — triggers task overview
//! - Clock display (centre-right)
//! - System indicator area (right)
//!
//! Complements the existing bottom taskbar (Xfce-style).

use core::sync::atomic::{AtomicU32, Ordering};

/// Top panel height in pixels.
pub const TOP_PANEL_HEIGHT: u32 = 26;

/// Global atomic: top panel enabled (1 = on, 0 = off).
/// Set by the kernel from SettingsContext.
static TOP_PANEL_ENABLED: AtomicU32 = AtomicU32::new(1);

/// Check whether the GNOME-style top panel is enabled.
#[inline]
pub fn is_top_panel_enabled() -> bool {
    TOP_PANEL_ENABLED.load(Ordering::Relaxed) != 0
}

/// Toggle the top panel on/off. Returns the new state.
pub fn toggle_top_panel() -> bool {
    let prev = TOP_PANEL_ENABLED.fetch_xor(1, Ordering::Relaxed);
    prev == 0
}

/// Set the top panel enabled state explicitly.
pub fn set_top_panel_enabled(on: bool) {
    TOP_PANEL_ENABLED.store(u32::from(on), Ordering::Relaxed);
}

/// Get a reference to the underlying atomic for kernel sync.
pub fn top_panel_enabled_atomic() -> &'static AtomicU32 {
    &TOP_PANEL_ENABLED
}

/// Top panel background colour.
pub const TOP_PANEL_BG: u32 = 0x0a0a14;

/// Top panel button colour.
pub const TOP_PANEL_BUTTON_BG: u32 = 0x222238;

/// Top panel button hover colour.
pub const TOP_PANEL_BUTTON_HOVER: u32 = 0x3A7BD5;

/// Top panel state (owned by Desktop / Solvent).
pub struct TopPanel {
    /// Clock text (set by solvent::update_clock).
    pub clock_text: alloc::string::String,
    /// Whether the Activities button is highlighted.
    pub activities_highlight: bool,
}

impl TopPanel {
    pub fn new() -> Self {
        Self {
            clock_text: alloc::string::String::new(),
            activities_highlight: false,
        }
    }

    /// Render the top panel onto the framebuffer.
    ///
    /// `fb_width` is the logical screen width; `fb_stride` is the actual
    /// pixels‑per‑scan‑line (may be larger on real hardware with GOP padding).
    pub fn render(&self, fb: &mut [u32], fb_width: u32, fb_height: u32, fb_stride: u32) {
        let colors = crate::theme::current_colors();
        let stride = fb_stride as usize;
        let fb_w = fb_width as usize;
        let panel_h = TOP_PANEL_HEIGHT;

        if fb_height < panel_h {
            return;
        }

        // Fill panel background
        for row in 0..panel_h {
            let rs = (row as usize) * stride;
            fb[rs..rs + fb_w].fill(colors.taskbar_bg);
        }

        // Draw "Activities" button (left side)
        let btn_bg = if self.activities_highlight {
            colors.active
        } else {
            colors.taskbar_inactive_bg
        };
        let btn_x = 4u32;
        let btn_y = 4u32;
        let btn_w = 88u32;
        let btn_h = panel_h - 8;

        for row in btn_y..btn_y + btn_h {
            let rs = (row as usize) * stride + btn_x as usize;
            let end = rs + btn_w as usize;
            if end <= fb.len() {
                fb[rs..end].fill(btn_bg);
            }
        }

        // "Activities" label
        let label = b"Activities";
        let tx = btn_x + 8;
        let ty = btn_y + 3;
        for (i, ch) in label.iter().enumerate() {
            if *ch < 32 || *ch > 126 {
                continue;
            }
            for gry in 0..12 {
                let py = ty + gry;
                if py >= fb_height {
                    continue;
                }
                for grx in 0..8 {
                    let px = tx + (i as u32) * 8 + grx;
                    if px >= fb_width {
                        continue;
                    }
                    if crate::font::get_glyph_pixel(*ch, gry as u32, grx as u32) {
                        let idx = (py as usize) * stride + px as usize;
                        if idx < fb.len() {
                            fb[idx] = colors.taskbar_text;
                        }
                    }
                }
            }
        }

        // Draw clock (centre-right)
        if !self.clock_text.is_empty() {
            let clock_bytes = self.clock_text.as_bytes();
            let clock_len = clock_bytes.len() as u32 * 8;
            let clock_x = fb_width.saturating_sub(clock_len + 12);
            let clock_y = (panel_h.saturating_sub(12)) / 2;

            for (i, ch) in clock_bytes.iter().enumerate() {
                if *ch < 32 || *ch > 126 {
                    continue;
                }
                for gry in 0..12 {
                    for grx in 0..8 {
                        let px = clock_x + (i as u32) * 8 + grx;
                        let py = clock_y + gry;
                        if px < fb_width && py < fb_height {
                            if crate::font::get_glyph_pixel(*ch, gry as u32, grx as u32) {
                                let idx = (py as usize) * stride + px as usize;
                                if idx < fb.len() {
                                    fb[idx] = colors.primary;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Hit-test the Activities button.
    pub fn hit_activities_button(&self, px: i32, py: i32) -> bool {
        let btn_x = 4i32;
        let btn_y = 4i32;
        let btn_w = 88i32;
        let btn_h = (TOP_PANEL_HEIGHT - 8) as i32;
        px >= btn_x && px < btn_x + btn_w && py >= btn_y && py < btn_y + btn_h
    }

    /// Check if a point is inside the top panel area.
    pub fn contains(&self, py: i32) -> bool {
        py < TOP_PANEL_HEIGHT as i32
    }
}
