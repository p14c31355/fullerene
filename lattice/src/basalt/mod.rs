//! Basalt: the original Fullerene desktop implementation.

use crate::common::{
    FrameButtons, LatticeStyle, Palette, ShellKind, StyleMetrics, StyleSpec, WindowVisualState,
};
use crate::painter::Painter;
use crate::window::Window;
use crate::{menu::PopupMenu, taskbar::Taskbar, top_panel::TopPanel};

static SPEC: StyleSpec = StyleSpec {
    name: "Basalt",
    kind: ShellKind::Basalt,
    palette: Palette {
        bg: 0x1B1B1D,
        surface: 0x242426,
        primary: 0x3584E4,
        active: 0x2A7DE0,
        text: 0xE0E0E0,
        muted: 0x888888,
        border_active: 0x3584E4,
        border_inactive: 0x555555,
        title_active: 0x2A2A2C,
        title_inactive: 0x333335,
        accent: 0xE6A817,
        danger: 0xD94A4A,
        taskbar_bg: 0x151516,
        taskbar_text: 0xCCCCCC,
        taskbar_active_bg: 0x3584E4,
        taskbar_inactive_bg: 0x2C2C2E,
        window_shadow: 0x000000,
        menu_bg: 0x242426,
        menu_border: 0x555555,
    },
    metrics: StyleMetrics {
        title_bar_height: 28,
        taskbar_height: 28,
        window_radius: 8,
        window_border: 1,
        top_panel_height: 26,
        title_buttons_on_left: false,
    },
};

pub struct BasaltStyle;
pub static STYLE: BasaltStyle = BasaltStyle;

pub fn style() -> &'static dyn LatticeStyle {
    &STYLE
}

impl LatticeStyle for BasaltStyle {
    fn spec(&self) -> &StyleSpec {
        &SPEC
    }

    fn draw_window_frame(
        &self,
        canvas: &mut Painter<'_>,
        window: &Window,
        state: WindowVisualState,
    ) {
        crate::common::draw_window_frame(canvas, window, state, &SPEC, FrameButtons::Square);
    }

    fn draw_menu(&self, canvas: &mut Painter<'_>, menu: &PopupMenu) {
        crate::common::draw_menu(canvas, menu, &SPEC);
    }

    fn draw_taskbar(&self, canvas: &mut Painter<'_>, taskbar: &Taskbar) {
        crate::common::draw_basalt_taskbar(canvas, taskbar, &SPEC);
    }

    fn draw_top_panel(&self, canvas: &mut Painter<'_>, panel: &TopPanel) {
        crate::common::draw_top_panel(canvas, panel, &SPEC);
    }
}
