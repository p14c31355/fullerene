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

        // "Activities" label using Painter TTF (no separate button background)
        let mut painter = crate::painter::Painter::new_with_stride(fb, fb_width, fb_height, fb_stride);
        let tx = 12i32;
        let ty = 4i32;
        let activities_color = if self.activities_highlight {
            colors.primary
        } else {
            colors.taskbar_text
        };
        painter.draw_text(tx, ty, "Activities", activities_color, 13.0);

        // Draw clock (centre-right) using Painter TTF
        if !self.clock_text.is_empty() {
            let clock_y = ((panel_h.saturating_sub(12)) / 2) as i32;
            let clock_x = (fb_width as i32).saturating_sub(120);
            painter.draw_text(clock_x, clock_y, &self.clock_text, colors.primary, 13.0);
        }
    }

    /// Hit-test the Activities button.
    pub fn hit_activities_button(&self, px: i32, py: i32) -> bool {
        let btn_x = 4i32;
        let btn_y = 2i32;
        let btn_w = 100i32;
        let btn_h = (TOP_PANEL_HEIGHT - 4) as i32;
        px >= btn_x && px < btn_x + btn_w && py >= btn_y && py < btn_y + btn_h
    }

    /// Check if a point is inside the top panel area.
    pub fn contains(&self, py: i32) -> bool {
        py < TOP_PANEL_HEIGHT as i32
    }
}
