//! Prism: a bright, focused shell with a centred Windows-like panel.

use crate::common::{
    FrameButtons, LatticeStyle, Palette, ShellKind, StyleMetrics, StyleSpec, WindowVisualState,
};
use crate::painter::Painter;
use crate::window::Window;
use crate::{menu::PopupMenu, taskbar::Taskbar, top_panel::TopPanel};

static SPEC: StyleSpec = StyleSpec {
    name: "Prism",
    kind: ShellKind::Prism,
    palette: Palette {
        bg: 0xB8D9F0,
        surface: 0xF7FAFD,
        primary: 0x2563EB,
        active: 0x1D4ED8,
        text: 0x17243A,
        muted: 0x60718A,
        border_active: 0x73A8E6,
        border_inactive: 0xA7BCD2,
        title_active: 0xF7FAFD,
        title_inactive: 0xE5EDF5,
        accent: 0xF59E0B,
        danger: 0xE5484D,
        taskbar_bg: 0xE7F0F8,
        taskbar_text: 0x243B53,
        taskbar_active_bg: 0xC7DCF1,
        taskbar_inactive_bg: 0xD8E5F0,
        window_shadow: 0x6483A0,
        menu_bg: 0xF7FAFD,
        menu_border: 0x9CB6D0,
    },
    metrics: StyleMetrics {
        title_bar_height: 32,
        taskbar_height: 54,
        window_radius: 12,
        window_border: 1,
        top_panel_height: 0,
        title_buttons_on_left: false,
    },
};

pub struct PrismStyle;
pub static STYLE: PrismStyle = PrismStyle;

pub fn style() -> &'static dyn LatticeStyle {
    &STYLE
}

impl LatticeStyle for PrismStyle {
    fn spec(&self) -> &StyleSpec {
        &SPEC
    }

    fn draw_window_frame(
        &self,
        canvas: &mut Painter<'_>,
        window: &Window,
        state: WindowVisualState,
    ) {
        crate::common::draw_window_frame(canvas, window, state, &SPEC, FrameButtons::Windows);
    }

    fn draw_menu(&self, canvas: &mut Painter<'_>, menu: &PopupMenu) {
        crate::common::draw_menu(canvas, menu, &SPEC);
    }

    fn draw_taskbar(&self, canvas: &mut Painter<'_>, taskbar: &Taskbar) {
        let width = canvas.width;
        let height = canvas.height;
        let bar_h = SPEC.metrics.taskbar_height;
        let bar_y = height.saturating_sub(bar_h);
        let palette = &SPEC.palette;
        canvas.fill_rect(0, bar_y as i32, width, bar_h, palette.taskbar_bg);

        // A centred launcher cluster gives Prism its Windows-like rhythm,
        // while the left launcher and right tray remain easy to discover.
        canvas.rounded_rect(16, bar_y as i32 + 9, 36, 36, 10, palette.primary);
        canvas.draw_text(27, bar_y as i32 + 19, "F", 0xFFFFFF, 14.0);
        for (index, entry) in taskbar.entries.iter().enumerate() {
            let Some((x, y, w, h)) =
                crate::style::taskbar_entry_rect(index, taskbar.entries.len(), width, height)
            else {
                continue;
            };
            let color = if entry.focused {
                palette.taskbar_active_bg
            } else {
                palette.taskbar_inactive_bg
            };
            canvas.rounded_rect(x, y, w, h, 10, color);
            canvas.draw_text(x + 10, y + 10, &entry.title, palette.taskbar_text, 12.0);
        }
        crate::network_menu::render_wifi_icon(
            canvas.fb,
            width,
            height,
            taskbar.wifi_icon_x(width),
            bar_y + 17,
            taskbar.wifi_connected,
            taskbar.wifi_visible,
            taskbar.wifi_signal,
        );
        if !taskbar.clock_text.is_empty() {
            canvas.draw_text(
                width.saturating_sub(100) as i32,
                bar_y as i32 + 20,
                &taskbar.clock_text,
                palette.taskbar_text,
                13.0,
            );
        }
    }

    fn draw_top_panel(&self, _canvas: &mut Painter<'_>, _panel: &TopPanel) {
        // Prism uses the bottom panel as its only chrome.
    }
}
