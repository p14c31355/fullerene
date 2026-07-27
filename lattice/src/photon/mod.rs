//! Photon: a light, airy shell with a top menu bar and floating dock.

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
        bg: 0xD9E9F5,
        surface: 0xF3F8FC,
        primary: 0x0A84FF,
        active: 0x007AFF,
        text: 0x10233D,
        muted: 0x5D7088,
        border_active: 0x6CB7F5,
        border_inactive: 0xA8C3D8,
        title_active: 0xEAF5FF,
        title_inactive: 0xD5E4F0,
        accent: 0xFF9F0A,
        danger: 0xFF453A,
        taskbar_bg: 0xDCECF7,
        taskbar_text: 0x17324F,
        taskbar_active_bg: 0xB9DBF5,
        taskbar_inactive_bg: 0xC9DCE9,
        window_shadow: 0x6B8294,
        menu_bg: 0xF8FBFE,
        menu_border: 0x9EB8CC,
    },
    metrics: StyleMetrics {
        title_bar_height: 34,
        taskbar_height: 78,
        window_radius: 14,
        window_border: 1,
        top_panel_height: 30,
        title_buttons_on_left: true,
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
        crate::common::draw_window_frame(canvas, window, state, &SPEC, FrameButtons::Capsule);
    }

    fn draw_menu(&self, canvas: &mut Painter<'_>, menu: &PopupMenu) {
        crate::common::draw_menu(canvas, menu, &SPEC);
    }

    fn draw_taskbar(&self, canvas: &mut Painter<'_>, taskbar: &Taskbar) {
        let width = canvas.width;
        let height = canvas.height;
        let bar_h = SPEC.metrics.taskbar_height;
        let bar_y = height.saturating_sub(bar_h);
        let dock_w = (taskbar.entries.len() as u32 * 56 + 24).max(240);
        let dock_x = width.saturating_sub(dock_w) / 2;
        let palette = &SPEC.palette;

        canvas.draw_shadow(
            dock_x as i32,
            bar_y as i32 + 7,
            dock_w,
            64,
            18,
            0,
            7,
            palette.window_shadow,
        );
        canvas.rounded_rect(
            dock_x as i32,
            bar_y as i32 + 7,
            dock_w,
            64,
            18,
            palette.taskbar_bg,
        );
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
            canvas.rounded_rect(x, y, w, h, 14, color);
            let label = entry.title.chars().next().unwrap_or('W');
            let mut encoded = [0u8; 4];
            canvas.draw_text(
                x + 15,
                y + 13,
                label.encode_utf8(&mut encoded),
                palette.taskbar_text,
                15.0,
            );
        }
        crate::network_menu::render_wifi_icon(
            canvas.fb,
            width,
            height,
            taskbar.wifi_icon_x(width),
            bar_y + 20,
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

    fn draw_top_panel(&self, canvas: &mut Painter<'_>, panel: &TopPanel) {
        crate::common::draw_top_panel(canvas, panel, &SPEC);
    }
}
