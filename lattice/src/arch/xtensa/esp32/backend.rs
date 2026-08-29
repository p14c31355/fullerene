//! Dirty-rectangle RGB565 framebuffer and the compact ESP32 Lattice desktop.
//!
//! The board's DRAM cannot afford the desktop's 32-bit RGBA pipeline, so this
//! backend is RGB565 end to end. It retains Lattice's owned-surface semantics
//! while adapting the desktop to the 320x240 panel.

use crate::font::glyph;
use alloc::vec::Vec;

const DISPLAY_WIDTH: u16 = 320;
const DISPLAY_HEIGHT: u16 = 240;

/// A permanent RGB565 surface is 150 KiB; on this profile it is cheaper and
/// simpler than double-buffering to SPI. All transfers use dirty clips.
pub struct Esp32Compositor {
    width: u16,
    height: u16,
    pixels: Vec<u16>,
}

impl Esp32Compositor {
    pub const fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            pixels: Vec::new(),
        }
    }

    /// Allocates the bounded RGB565 surface after the kernel heap is ready.
    pub fn allocate(&mut self) -> bool {
        let count = usize::from(self.width) * usize::from(self.height);
        self.pixels = alloc::vec![0u16; count];
        self.pixels.len() == count
    }

    pub fn dimensions(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    pub fn pixels(&self) -> &[u16] {
        &self.pixels
    }

    pub fn clear(&mut self, color: u16) {
        self.pixels.fill(color);
    }

    pub fn mark_and_flush(
        &mut self,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        color: u16,
    ) -> Option<DirtyClip> {
        if x >= self.width || y >= self.height || self.pixels.is_empty() {
            return None;
        }
        let width = width.min(self.width - x);
        let height = height.min(self.height - y);
        for row in 0..height {
            let start = usize::from(y + row) * usize::from(self.width) + usize::from(x);
            self.pixels[start..start + usize::from(width)].fill(color);
        }
        Some(DirtyClip {
            x,
            y,
            width,
            height,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirtyClip {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// Compact applications exposed by the first ESP32 Lattice desktop. Entries
/// are real registration points; their initial views are bring-up views.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbeddedApp {
    SystemInfo,
    Nozzle,
    Files,
    Settings,
}

impl EmbeddedApp {
    pub const ALL: [Self; 4] = [Self::SystemInfo, Self::Nozzle, Self::Files, Self::Settings];

    pub const fn title(self) -> &'static str {
        match self {
            Self::SystemInfo => "System",
            Self::Nozzle => "Nozzle",
            Self::Files => "Files",
            Self::Settings => "Settings",
        }
    }

    pub const fn summary(self) -> &'static str {
        match self {
            Self::SystemInfo => "kernel bring-up view",
            Self::Nozzle => "serial shell pending",
            Self::Files => "Genome not mounted",
            Self::Settings => "not persisted yet",
        }
    }
}

const PANEL_BG: u16 = 0x10a4;
const DESKTOP_TOP: u16 = 0x18c5;
const DESKTOP_BOTTOM: u16 = 0x0841;
const CARD_BG: u16 = 0x18c6;
const CARD_BORDER: u16 = 0x31a6;
const ACCENT: u16 = 0x4fdd;
const TEXT: u16 = 0xffff;
const MUTED: u16 = 0x8c74;

/// Lattice's small-screen desktop state. It is deliberately compact enough to
/// live in a task stack while composing into the shared RGB565 surface.
pub struct EmbeddedDesktop {
    active: EmbeddedApp,
}

impl EmbeddedDesktop {
    pub const fn new() -> Self {
        Self {
            active: EmbeddedApp::SystemInfo,
        }
    }

    pub fn active(&self) -> EmbeddedApp {
        self.active
    }

    pub fn set_active(&mut self, app: EmbeddedApp) -> bool {
        if self.active == app {
            return false;
        }
        self.active = app;
        true
    }

    pub fn hit_taskbar(&self, x: u16, y: u16) -> Option<EmbeddedApp> {
        if !(216..=239).contains(&y) {
            return None;
        }
        EmbeddedApp::ALL.iter().copied().find(|app| {
            let Some(rect) = taskbar_rect(*app) else {
                return false;
            };
            x >= rect.0 && x < rect.0 + rect.2 && y >= rect.1 && y < rect.1 + rect.3
        })
    }

    pub fn render(&mut self, target: &mut Esp32Compositor) {
        if target.dimensions() != (DISPLAY_WIDTH, DISPLAY_HEIGHT) || target.pixels().is_empty() {
            return;
        }
        self.draw_wallpaper(target);
        self.draw_top_panel(target);
        self.draw_window(target);
        self.draw_taskbar(target);
    }

    fn draw_wallpaper(&mut self, target: &mut Esp32Compositor) {
        for y in 0..DISPLAY_HEIGHT {
            let weight = u16::from(y) * 100 / u16::from(DISPLAY_HEIGHT);
            let color = DESKTOP_TOP
                .saturating_sub((DESKTOP_TOP.saturating_sub(DESKTOP_BOTTOM)) * weight / 100);
            target.mark_and_flush(0, y, DISPLAY_WIDTH, 1, color);
        }
    }

    fn draw_top_panel(&mut self, target: &mut Esp32Compositor) {
        target.mark_and_flush(0, 0, DISPLAY_WIDTH, 22, PANEL_BG);
        target.mark_and_flush(0, 22, DISPLAY_WIDTH, 1, CARD_BORDER);
        self.draw_text(target, 8, 6, "FullereneOS", TEXT);
        self.draw_text(target, 206, 6, "Lattice", MUTED);
    }

    fn draw_window(&mut self, target: &mut Esp32Compositor) {
        let (x, y, width, height) = (8, 30, 304, 176);
        target.mark_and_flush(x, y, width, height, CARD_BG);
        target.mark_and_flush(x, y, width, 1, CARD_BORDER);
        target.mark_and_flush(x, y + height - 1, width, 1, CARD_BORDER);
        target.mark_and_flush(x, y, 1, height, CARD_BORDER);
        target.mark_and_flush(x + width - 1, y, 1, height, CARD_BORDER);

        target.mark_and_flush(x + 1, y + 1, width - 2, 18, PANEL_BG);
        self.draw_text(target, x + 8, y + 4, self.active.title(), TEXT);

        let content_y = y + 26;
        self.draw_text(target, x + 10, content_y, self.active.summary(), ACCENT);
        target.mark_and_flush(x + 10, content_y + 14, 284, 1, CARD_BORDER);

        match self.active {
            EmbeddedApp::SystemInfo => {
                self.draw_info(target, x + 10, content_y + 24, "CPU", "ESP32 Xtensa LX6");
                self.draw_info(target, x + 10, content_y + 42, "Panel", "320x240 RGB565");
                self.draw_info(
                    target,
                    x + 10,
                    content_y + 60,
                    "Profile",
                    "single-address-space",
                );
                self.draw_info(target, x + 10, content_y + 78, "Scheduler", "cooperative");
                self.draw_info(target, x + 10, content_y + 96, "Input", "XPT2046 probe");
            }
            EmbeddedApp::Nozzle => {
                self.draw_info(target, x + 10, content_y + 24, "State", "registered");
                self.draw_info(target, x + 10, content_y + 42, "Transport", "UART bring-up");
                self.draw_text(
                    target,
                    x + 10,
                    content_y + 68,
                    "Commands are staged in Nozzle.",
                    MUTED,
                );
            }
            EmbeddedApp::Files => {
                self.draw_info(target, x + 10, content_y + 24, "State", "not mounted");
                self.draw_info(target, x + 10, content_y + 42, "Backend", "Genome SD");
                self.draw_text(target, x + 10, content_y + 68, "Storage comes next.", MUTED);
            }
            EmbeddedApp::Settings => {
                self.draw_info(target, x + 10, content_y + 24, "State", "volatile");
                self.draw_info(target, x + 10, content_y + 42, "Theme", "Lattice compact");
                self.draw_text(
                    target,
                    x + 10,
                    content_y + 68,
                    "Persistence is planned.",
                    MUTED,
                );
            }
        }
    }

    fn draw_info(&mut self, target: &mut Esp32Compositor, x: u16, y: u16, key: &str, value: &str) {
        self.draw_text(target, x, y, key, MUTED);
        self.draw_text(target, x + 52, y, value, TEXT);
    }

    fn draw_taskbar(&mut self, target: &mut Esp32Compositor) {
        target.mark_and_flush(0, 214, DISPLAY_WIDTH, 26, PANEL_BG);
        target.mark_and_flush(0, 214, DISPLAY_WIDTH, 1, CARD_BORDER);
        for app in EmbeddedApp::ALL {
            let Some((x, y, width, height)) = taskbar_rect(app) else {
                continue;
            };
            let selected = app == self.active;
            target.mark_and_flush(x, y, width, height, if selected { ACCENT } else { 0x18e7 });
            target.mark_and_flush(x, y, width, 1, CARD_BORDER);
            target.mark_and_flush(x, y + height - 1, width, 1, CARD_BORDER);
            target.mark_and_flush(x, y, 1, height, CARD_BORDER);
            target.mark_and_flush(x + width - 1, y, 1, height, CARD_BORDER);
            let text_width = u16::try_from(app.title().len() * 8).unwrap_or(width);
            self.draw_text(
                target,
                x + (width.saturating_sub(text_width)) / 2,
                y + 5,
                app.title(),
                if selected { 0x0000 } else { TEXT },
            );
        }
    }

    fn draw_text(&mut self, target: &mut Esp32Compositor, x: u16, y: u16, text: &str, color: u16) {
        for (column, character) in text.bytes().enumerate() {
            let glyph_x = x + u16::try_from(column * 8).unwrap_or(0);
            if glyph_x >= DISPLAY_WIDTH {
                break;
            }
            let glyph = glyph(character);
            for row in 0..13u16 {
                for pixel in 0..8u16 {
                    if glyph.pixel(u32::from(row), u32::from(pixel)) {
                        target.mark_and_flush(glyph_x + pixel, y + row, 1, 1, color);
                    }
                }
            }
        }
    }
}

fn taskbar_rect(app: EmbeddedApp) -> Option<(u16, u16, u16, u16)> {
    let index = EmbeddedApp::ALL
        .iter()
        .position(|candidate| *candidate == app)?;
    let width = 70u16;
    let spacing = (DISPLAY_WIDTH - width * 4) / 5;
    Some((spacing + index as u16 * (width + spacing), 218, width, 18))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_dirty_rectangles_to_the_panel() {
        let mut target = Esp32Compositor::new(320, 240);
        assert!(target.allocate());
        let clip = target.mark_and_flush(300, 200, 40, 40, 0x1234).unwrap();
        assert_eq!(
            (clip.x, clip.y, clip.width, clip.height),
            (300, 200, 20, 40)
        );
        assert!(target.mark_and_flush(320, 0, 1, 1, 0).is_none());
    }

    #[test]
    fn fills_only_visible_pixels() {
        let mut target = Esp32Compositor::new(2, 2);
        assert!(target.allocate());
        target.mark_and_flush(1, 1, 4, 4, 0x1234);
        assert_eq!(target.pixels(), &[0, 0, 0, 0x1234]);
    }

    #[test]
    fn desktop_selects_only_taskbar_hits() {
        let mut desktop = EmbeddedDesktop::new();
        assert_eq!(desktop.active(), EmbeddedApp::SystemInfo);
        assert_eq!(desktop.hit_taskbar(0, 100), None);
        assert_eq!(desktop.hit_taskbar(0, 240), None);
        assert_eq!(desktop.hit_taskbar(8, 226), Some(EmbeddedApp::SystemInfo));
        assert_eq!(desktop.hit_taskbar(250, 226), Some(EmbeddedApp::Settings));
        assert!(desktop.set_active(EmbeddedApp::Settings));
        assert!(!desktop.set_active(EmbeddedApp::Settings));
    }
}
