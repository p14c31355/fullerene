//! Desktop icon layer for Fullerene OS (Xfce-style).
//!
//! Renders clickable icons on the desktop background layer,
//! with labels and hit-testing for mouse events.

use crate::painter::Painter;
use alloc::string::String;
use alloc::vec::Vec;

/// A single desktop icon entry.
#[derive(Debug, Clone)]
pub struct DesktopIcon {
    /// Display name under the icon.
    pub label: String,
    /// Position (top-left of icon area).
    pub x: i32,
    pub y: i32,
    /// Icon size in pixels.
    pub size: u32,
    /// Background colour of the icon box.
    pub color: u32,
}

/// Collection of desktop icons with hit-testing support.
pub struct DesktopIconLayer {
    pub icons: Vec<DesktopIcon>,
    /// Grid spacing between icons.
    pub grid_spacing: u32,
}

impl DesktopIconLayer {
    pub fn new() -> Self {
        let icons = alloc::vec![
            DesktopIcon {
                label: String::from("Shell"),
                x: 24,
                y: 32,
                size: 64,
                color: 0x333344,
            },
            DesktopIcon {
                label: String::from("Files"),
                x: 112,
                y: 32,
                size: 64,
                color: 0x334433,
            },
            DesktopIcon {
                label: String::from("Settings"),
                x: 200,
                y: 32,
                size: 64,
                color: 0x444433,
            },
            DesktopIcon {
                label: String::from("About"),
                x: 24,
                y: 136,
                size: 64,
                color: 0x443344,
            }
        ];

        Self {
            icons,
            grid_spacing: 88,
        }
    }

    /// Get the pre-rendered SVG icon surface for a desktop icon index.
    fn icon_surface(idx: usize) -> Option<crate::surface::Surface> {
        match idx {
            0 => Some(crate::icon::ICON_SHELL.surface()),
            1 => Some(crate::icon::ICON_FILES.surface()),
            2 => Some(crate::icon::ICON_SETTINGS.surface()),
            3 => Some(crate::icon::ICON_ABOUT.surface()),
            _ => None,
        }
    }

    /// Find an icon at the given screen position.
    /// Returns the index in `self.icons` or `None`.
    pub fn hit_test(&self, px: i32, py: i32) -> Option<usize> {
        let label_h: i32 = 18;
        for (i, icon) in self.icons.iter().enumerate() {
            let ix = icon.x;
            let iy = icon.y;
            if px >= ix
                && px < ix + icon.size as i32
                && py >= iy
                && py < iy + icon.size as i32 + label_h
            {
                return Some(i);
            }
        }
        None
    }

    /// Render all icons into the framebuffer, clipped to a dirty rect.
    pub fn render(
        &self,
        fb: &mut [u32],
        fb_width: u32,
        fb_height: u32,
        clip_x: u32,
        clip_y: u32,
        clip_w: u32,
        clip_h: u32,
    ) {
        let mut painter = Painter::new(fb, fb_width, fb_height);
        painter.clip_rect(clip_x as i32, clip_y as i32, clip_w, clip_h);
        for (idx, icon) in self.icons.iter().enumerate() {
            // Draw SVG icon if available, else fall back to rounded color box
            if let Some(svg_surface) = Self::icon_surface(idx) {
                painter.blit_surface(&svg_surface, icon.x, icon.y);
            } else {
                painter.rounded_rect(icon.x, icon.y, icon.size, icon.size, 8, icon.color);
            }

            // Draw label below the icon using painter text
            let lx = icon.x + 2;
            let ly = icon.y + icon.size as i32 + 6;
            painter.draw_text(lx, ly, &icon.label, crate::compositor::COLOR_TEXT, 13.0);
        }
    }
}
