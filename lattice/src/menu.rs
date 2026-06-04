//! Menu system — system menu, context menu, and popup overlays.
//!
//! Provides:
//! - `SystemMenu` — a popup menu triggered from the taskbar
//! - `ContextMenu` — right‑click context menu on the desktop
//! - Rendering as overlay rectangles on the compositor

use crate::scene::OverlayRect;

/// Menu item entry.
#[derive(Debug, Clone)]
pub struct MenuItem {
    /// Display label.
    pub label: alloc::string::String,
    /// Action identifier (matched by the runtime).
    pub action: alloc::string::String,
}

/// A popup menu with position and items.
#[derive(Debug, Clone)]
pub struct PopupMenu {
    /// Screen position (top‑left corner).
    pub x: u32,
    pub y: u32,
    /// Menu width.
    pub width: u32,
    /// Height computed from item count × ITEM_HEIGHT.
    pub height: u32,
    /// Menu items.
    pub items: alloc::vec::Vec<MenuItem>,
    /// Whether the menu is currently visible.
    pub visible: bool,
}

/// Height of a single menu item row in pixels.
pub const ITEM_HEIGHT: u32 = 20;
/// Menu border width.
pub const MENU_BORDER: u32 = 1;
/// Menu background colour.
pub const MENU_BG: u32 = 0x2A2A3E;
/// Menu item hover colour.
pub const MENU_HOVER: u32 = 0x3A7BD5;
/// Menu text colour.
pub const MENU_TEXT: u32 = 0xCCCCCC;
/// Menu border colour.
pub const MENU_BORDER_COLOR: u32 = 0x4A90D9;

/// Minimum menu width in pixels.
pub const MENU_MIN_WIDTH: u32 = 120;

impl PopupMenu {
    /// Create a new popup menu at the given position with the given items.
    pub fn new(x: u32, y: u32, items: alloc::vec::Vec<MenuItem>) -> Self {
        let height = items.len() as u32 * ITEM_HEIGHT + MENU_BORDER * 2;
        // Compute width from longest label
        let max_label = items.iter().map(|i| i.label.len()).max().unwrap_or(10);
        let width = ((max_label as u32 * 8) + 16).max(MENU_MIN_WIDTH);
        Self {
            x,
            y,
            width,
            height,
            items,
            visible: true,
        }
    }

    /// Check if a screen point hits this menu.
    pub fn hit_test(&self, px: i32, py: i32) -> Option<usize> {
        if !self.visible {
            return None;
        }
        let x = self.x as i32;
        let y = self.y as i32;
        let w = self.width as i32;
        if px < x || px >= x + w {
            return None;
        }
        let rel_y = py - y - MENU_BORDER as i32;
        if rel_y < 0 {
            return None;
        }
        let idx = (rel_y as u32) / ITEM_HEIGHT;
        if idx < self.items.len() as u32 {
            Some(idx as usize)
        } else {
            None
        }
    }

    /// Generate overlay rectangles for rendering.
    pub fn to_overlays(&self) -> alloc::vec::Vec<OverlayRect> {
        let mut rects = alloc::vec::Vec::new();
        if !self.visible {
            return rects;
        }

        // Background
        rects.push(OverlayRect {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            color: MENU_BG,
        });

        // Items
        for (i, item) in self.items.iter().enumerate() {
            let item_y = self.y + MENU_BORDER + i as u32 * ITEM_HEIGHT;
            // Item background
            rects.push(OverlayRect {
                x: self.x + MENU_BORDER,
                y: item_y,
                width: self.width - MENU_BORDER * 2,
                height: ITEM_HEIGHT,
                color: MENU_BG,
            });
        }

        rects
    }

    /// Render menu text onto a framebuffer (called by compositor overlay pass).
    pub fn render_text(&self, fb: &mut [u32], fbw: u32, fbh: u32) {
        if !self.visible {
            return;
        }
        for (i, item) in self.items.iter().enumerate() {
            let item_y = self.y + MENU_BORDER + i as u32 * ITEM_HEIGHT;
            let tx = self.x + MENU_BORDER + 4;
            let ty = item_y + 4;
            for (j, ch) in item.label.bytes().enumerate() {
                if ch < 32 || ch > 126 {
                    continue;
                }
                for row in 0..12 {
                    let py = ty + row;
                    if py >= fbh {
                        continue;
                    }
                    for col in 0..8 {
                        let px = tx + (j as u32) * 8 + col;
                        if px >= fbw {
                            continue;
                        }
                        if crate::font::get_glyph_pixel(ch, row, col) {
                            let idx = (py as usize) * (fbw as usize) + px as usize;
                            if idx < fb.len() {
                                fb[idx] = MENU_TEXT;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// System menu (triggered from taskbar button).
pub fn system_menu_items() -> alloc::vec::Vec<MenuItem> {
    alloc::vec![
        MenuItem {
            label: alloc::string::String::from("About Fullerene"),
            action: alloc::string::String::from("about")
        },
        MenuItem {
            label: alloc::string::String::from("System Info"),
            action: alloc::string::String::from("sysinfo")
        },
        MenuItem {
            label: alloc::string::String::from("Shutdown"),
            action: alloc::string::String::from("shutdown")
        },
        MenuItem {
            label: alloc::string::String::from("Reboot"),
            action: alloc::string::String::from("reboot")
        },
    ]
}

/// Desktop context menu (right‑click on desktop).
pub fn desktop_context_menu() -> alloc::vec::Vec<MenuItem> {
    alloc::vec![
        MenuItem {
            label: alloc::string::String::from("New Terminal"),
            action: alloc::string::String::from("new_terminal")
        },
        MenuItem {
            label: alloc::string::String::from("Refresh"),
            action: alloc::string::String::from("refresh")
        },
        MenuItem {
            label: alloc::string::String::from("About"),
            action: alloc::string::String::from("about")
        },
    ]
}
