//! Prism: a bright, high-density desktop with a Windows 11-like centred
//! taskbar and crisp application cards.

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
        bg: 0xF3F5F8,
        surface: 0xFFFFFF,
        primary: 0x2563EB,
        active: 0x1D4ED8,
        text: 0x1F2937,
        muted: 0x667085,
        border_active: 0xB7C3D0,
        border_inactive: 0xD0D5DD,
        title_active: 0xFFFFFF,
        title_inactive: 0xF2F4F7,
        accent: 0xF59E0B,
        danger: 0xD92D20,
        taskbar_bg: 0xE8ECF2,
        taskbar_text: 0x344054,
        taskbar_active_bg: 0xD4E2FF,
        taskbar_inactive_bg: 0xF5F7FA,
        window_shadow: 0x98A2B3,
        menu_bg: 0xFFFFFF,
        menu_border: 0xD0D5DD,
    },
    metrics: StyleMetrics {
        title_bar_height: 30,
        taskbar_height: 50,
        window_radius: 8,
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
        canvas.fill_rect(0, bar_y as i32, width, 1, palette.border_inactive);

        for (index, route) in crate::common::PRISM_LAUNCHER_ROUTES
            .iter()
            .copied()
            .enumerate()
        {
            if let Some((x, y, w, h)) =
                crate::style::launcher_entry_rect(index, taskbar.entries.len(), width, height)
            {
                let bg = if index == 0 {
                    palette.primary
                } else {
                    palette.taskbar_inactive_bg
                };
                canvas.rounded_rect(x, y, w, h, 8, bg);
                crate::common::icon_for_route(route).blit_scaled_into(
                    canvas.fb,
                    width,
                    canvas.stride as usize,
                    x + 6,
                    y + 2,
                    32,
                );
            }
        }
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
            canvas.rounded_rect(x, y, w, h, 8, color);
            crate::common::task_icon(&entry.title).blit_scaled_into(
                canvas.fb,
                width,
                canvas.stride as usize,
                x + 6,
                y + 2,
                32,
            );
        }
        let tray_x = taskbar.status_group_x(width).min(width.saturating_sub(152));
        let tray_w = width.saturating_sub(tray_x);
        canvas.rounded_rect(
            tray_x as i32,
            bar_y as i32 + 7,
            tray_w,
            36,
            8,
            palette.taskbar_inactive_bg,
        );
        crate::common::draw_debug_status(
            canvas,
            taskbar,
            tray_x as i32 + 8,
            bar_y as i32 + 17,
            taskbar.wifi_icon_x(width).saturating_sub(8),
            palette.taskbar_text,
        );
        crate::network_menu::render_wifi_icon(
            canvas.fb,
            width,
            height,
            taskbar.wifi_icon_x(width),
            bar_y + 15,
            taskbar.wifi_connected,
            taskbar.wifi_visible,
            taskbar.wifi_signal,
        );
        crate::taskbar::render_power_icon(
            canvas,
            taskbar.power_icon_x(width) + 6,
            bar_y + 13,
            palette.taskbar_text,
        );
        if !taskbar.clock_text.is_empty() {
            canvas.draw_text(
                taskbar.clock_x(width) as i32,
                bar_y as i32 + 17,
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
