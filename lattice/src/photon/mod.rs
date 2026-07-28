//! Photon: the dark, panel-and-dock shell used by the Fullerene reference
//! desktop. Application surfaces stay bright so windows remain readable over
//! photographic wallpapers.

use crate::common::{
    FrameButtons, LatticeStyle, Palette, ShellKind, StyleMetrics, StyleSpec, WindowVisualState,
};
use crate::painter::Painter;
use crate::window::Window;
use crate::{menu::PopupMenu, taskbar::Taskbar, top_panel::TopPanel};

static SPEC: StyleSpec = StyleSpec {
    name: "Photon",
    kind: ShellKind::Photon,
    palette: Palette {
        bg: 0x10151B,
        surface: 0xF6F7F8,
        primary: 0x62A0EA,
        active: 0x2BAE66,
        text: 0x2E3436,
        muted: 0x5E6670,
        border_active: 0xD0D4D8,
        border_inactive: 0xC2C7CC,
        title_active: 0xE5E7E9,
        title_inactive: 0xD7DADD,
        accent: 0xE5A50A,
        danger: 0xE01B24,
        taskbar_bg: 0x252A33,
        taskbar_text: 0xEDF0F2,
        taskbar_active_bg: 0x3D6A91,
        taskbar_inactive_bg: 0x303641,
        window_shadow: 0x050608,
        // Photon popup menus follow its GTK-like light surfaces rather than
        // reusing the dark desktop/dock background.
        menu_bg: 0xF6F7F8,
        menu_border: 0xC2C7CC,
    },
    metrics: StyleMetrics {
        title_bar_height: 32,
        taskbar_height: 64,
        window_radius: 12,
        window_border: 1,
        top_panel_height: 0,
        title_buttons_on_left: false,
    },
};

pub struct PhotonStyle;
pub static STYLE: PhotonStyle = PhotonStyle;

pub fn style() -> &'static dyn LatticeStyle {
    &STYLE
}

impl LatticeStyle for PhotonStyle {
    fn spec(&self) -> &StyleSpec {
        &SPEC
    }

    fn draw_window_frame(
        &self,
        canvas: &mut Painter<'_>,
        window: &Window,
        state: WindowVisualState,
    ) {
        crate::common::draw_window_frame(canvas, window, state, &SPEC, FrameButtons::Gtk);
    }

    fn draw_menu(&self, canvas: &mut Painter<'_>, menu: &PopupMenu) {
        crate::common::draw_menu(canvas, menu, &SPEC);
    }

    fn draw_taskbar(&self, canvas: &mut Painter<'_>, taskbar: &Taskbar) {
        let width = canvas.width;
        let height = canvas.height;
        let bar_h = SPEC.metrics.taskbar_height;
        let bar_y = height.saturating_sub(bar_h);
        let dock_w = ((taskbar.entries.len() + crate::common::PHOTON_LAUNCHER_COUNT) as u32 * 48
            + 24)
            .max(360);
        let dock_x = 16u32;
        let palette = &SPEC.palette;

        canvas.draw_shadow(
            dock_x as i32,
            bar_y as i32 + 7,
            dock_w,
            50,
            18,
            0,
            7,
            palette.window_shadow,
        );
        canvas.rounded_rect(
            dock_x as i32,
            bar_y as i32 + 7,
            dock_w,
            50,
            18,
            palette.taskbar_bg,
        );
        for (index, route) in crate::common::PHOTON_LAUNCHER_ROUTES
            .iter()
            .copied()
            .enumerate()
        {
            if let Some((x, y, w, h)) =
                crate::style::launcher_entry_rect(index, taskbar.entries.len(), width, height)
            {
                if index == 0 {
                    canvas.rounded_rect(x, y, w, h, 8, palette.taskbar_inactive_bg);
                }
                crate::common::icon_for_route(route).blit_scaled_into(
                    canvas.fb,
                    width,
                    canvas.stride as usize,
                    x + 4,
                    y + 4,
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
            if entry.focused {
                canvas.rounded_rect(x, y, w, h, 8, color);
            }
            crate::common::task_icon(&entry.title).blit_scaled_into(
                canvas.fb,
                width,
                canvas.stride as usize,
                x + 4,
                y + 4,
                32,
            );
        }
        let tray_x = width.saturating_sub(168);
        canvas.rounded_rect(
            tray_x as i32,
            bar_y as i32 + 12,
            152,
            40,
            12,
            palette.taskbar_bg,
        );
        crate::network_menu::render_wifi_icon(
            canvas.fb,
            width,
            height,
            tray_x + 14,
            bar_y + 22,
            taskbar.wifi_connected,
            taskbar.wifi_visible,
            taskbar.wifi_signal,
        );
        if !taskbar.clock_text.is_empty() {
            canvas.draw_text(
                tray_x as i32 + 52,
                bar_y as i32 + 25,
                &taskbar.clock_text,
                palette.taskbar_text,
                13.0,
            );
        }
    }

    fn draw_top_panel(&self, canvas: &mut Painter<'_>, panel: &TopPanel) {
        let _ = (canvas, panel);
    }
}
