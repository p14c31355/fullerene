//! Desktop icon layer for Fullerene OS (Xfce-style).
//!
//! Renders clickable icons on the desktop background layer,
//! with labels and hit-testing for mouse events.

use crate::compositor::COLOR_TEXT;
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
        }];

        Self {
            icons,
            grid_spacing: 88,
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
        let fb_w = fb_width as usize;
        let cex = (clip_x + clip_w) as i32;
        let cey = (clip_y + clip_h) as i32;

        for icon in &self.icons {
            // Draw icon box
            for dy in 0..icon.size as i32 {
                let py = icon.y + dy;
                if py < clip_y as i32 || py >= cey || py >= fb_height as i32 {
                    continue;
                }
                for dx in 0..icon.size as i32 {
                    let px = icon.x + dx;
                    if px < clip_x as i32 || px >= cex || px >= fb_width as i32 {
                        continue;
                    }
                    let is_border = dy == 0
                        || dy == icon.size as i32 - 1
                        || dx == 0
                        || dx == icon.size as i32 - 1;
                    let color = if is_border {
                        crate::compositor::COLOR_PRIMARY
                    } else {
                        icon.color
                    };
                    let idx = (py as usize) * fb_w + px as usize;
                    if idx < fb.len() {
                        fb[idx] = color;
                    }
                }
            }

            // Draw label below the icon
            let label_y = icon.y + icon.size as i32 + 2;
            let label_x = icon.x + 2;
            for (ci, ch) in icon.label.bytes().enumerate() {
                if ch < 32 || ch > 126 {
                    continue;
                }
                for gry in 0..12 {
                    let py = label_y + gry;
                    if py < 0 || py >= fb_height as i32 || py < clip_y as i32 || py >= cey {
                        continue;
                    }
                    for grx in 0..8 {
                        let px = label_x + (ci as i32) * 8 + grx;
                        if px < 0 || px >= fb_width as i32 || px < clip_x as i32 || px >= cex {
                            continue;
                        }
                        if crate::font::get_glyph_pixel(ch, gry as u32, grx as u32) {
                            let idx = (py as usize) * fb_w + px as usize;
                            if idx < fb.len() {
                                fb[idx] = COLOR_TEXT;
                            }
                        }
                    }
                }
            }
        }
    }
}
