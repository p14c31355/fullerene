//! Shared visual contracts for Lattice implementations.
//!
//! Window management and input handling deliberately live outside these
//! types. A shell implementation only describes the geometry and paint
//! language used by the compositor.

use crate::menu::PopupMenu;
use crate::painter::Painter;
use crate::taskbar::Taskbar;
use crate::top_panel::TopPanel;
use crate::window::Window;

pub const PHOTON_LAUNCHER_COUNT: usize = 7;
pub const PRISM_LAUNCHER_COUNT: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Basalt,
    Photon,
    Prism,
}

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub bg: u32,
    pub surface: u32,
    pub primary: u32,
    pub active: u32,
    pub text: u32,
    pub muted: u32,
    pub border_active: u32,
    pub border_inactive: u32,
    pub title_active: u32,
    pub title_inactive: u32,
    pub accent: u32,
    pub danger: u32,
    pub taskbar_bg: u32,
    pub taskbar_text: u32,
    pub taskbar_active_bg: u32,
    pub taskbar_inactive_bg: u32,
    pub window_shadow: u32,
    pub menu_bg: u32,
    pub menu_border: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct StyleMetrics {
    pub title_bar_height: u32,
    pub taskbar_height: u32,
    pub window_radius: u32,
    pub window_border: u32,
    pub top_panel_height: u32,
    pub title_buttons_on_left: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct StyleSpec {
    pub name: &'static str,
    pub kind: ShellKind,
    pub palette: Palette,
    pub metrics: StyleMetrics,
}

/// The visual state needed by a window decoration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowVisualState {
    pub focused: bool,
    pub maximized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeHit {
    None,
    Move,
    Close,
    Minimize,
    Maximize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppRoute {
    Shell,
    Files,
    Terminal,
    Editor,
    Clock,
    Settings,
    About,
    Unknown,
}

pub const APP_GRID_ROUTES: [AppRoute; 7] = [
    AppRoute::Shell,
    AppRoute::Terminal,
    AppRoute::Editor,
    AppRoute::Clock,
    AppRoute::Settings,
    AppRoute::Files,
    AppRoute::About,
];

pub const PHOTON_LAUNCHER_ROUTES: [AppRoute; PHOTON_LAUNCHER_COUNT] = [
    AppRoute::Shell,
    AppRoute::Files,
    AppRoute::Terminal,
    AppRoute::Editor,
    AppRoute::Clock,
    AppRoute::Settings,
    AppRoute::About,
];

pub const PRISM_LAUNCHER_ROUTES: [AppRoute; PRISM_LAUNCHER_COUNT] = [
    AppRoute::Shell,
    AppRoute::Files,
    AppRoute::Terminal,
    AppRoute::Settings,
];

pub const fn route_label(route: AppRoute) -> &'static str {
    match route {
        AppRoute::Shell => "Shell",
        AppRoute::Files => "File Mgr",
        AppRoute::Terminal => "Terminal",
        AppRoute::Editor => "Editor",
        AppRoute::Clock => "Clock",
        AppRoute::Settings => "Settings",
        AppRoute::About => "About",
        AppRoute::Unknown => "Unknown",
    }
}

pub fn app_grid_route(index: usize) -> Option<AppRoute> {
    APP_GRID_ROUTES.get(index).copied()
}

pub fn route_for_name(name: &str) -> AppRoute {
    match name {
        "Shell" => AppRoute::Shell,
        "Files" | "File Mgr" | "File Manager" => AppRoute::Files,
        "Terminal" => AppRoute::Terminal,
        "Editor" => AppRoute::Editor,
        "Clock" => AppRoute::Clock,
        "Settings" => AppRoute::Settings,
        "About" => AppRoute::About,
        _ => AppRoute::Unknown,
    }
}

pub fn icon_for_route(route: AppRoute) -> &'static crate::icon::SvgIcon {
    match route {
        AppRoute::Shell => &crate::icon::ICON_SHELL,
        AppRoute::Files => &crate::icon::ICON_FILES,
        AppRoute::Terminal => &crate::icon::ICON_TERMINAL,
        AppRoute::Editor => &crate::icon::ICON_EDITOR,
        AppRoute::Clock => &crate::icon::ICON_CLOCK,
        AppRoute::Settings => &crate::icon::ICON_SETTINGS,
        AppRoute::About => &crate::icon::ICON_ABOUT,
        AppRoute::Unknown => &crate::icon::ICON_TERMINAL,
    }
}

/// Map a window/application title to one of the build-time icon assets used
/// by the Photon dock and Prism taskbar.
pub fn task_icon(title: &str) -> &'static crate::icon::SvgIcon {
    let route = match route_for_name(title) {
        AppRoute::Unknown => {
            let lower = title.as_bytes();
            if lower
                .windows(8)
                .any(|part| part.eq_ignore_ascii_case(b"settings"))
            {
                AppRoute::Settings
            } else if lower
                .windows(4)
                .any(|part| part.eq_ignore_ascii_case(b"file"))
            {
                AppRoute::Files
            } else if lower
                .windows(6)
                .any(|part| part.eq_ignore_ascii_case(b"editor"))
            {
                AppRoute::Editor
            } else if lower
                .windows(4)
                .any(|part| part.eq_ignore_ascii_case(b"shell"))
            {
                AppRoute::Shell
            } else if lower
                .windows(5)
                .any(|part| part.eq_ignore_ascii_case(b"about"))
            {
                AppRoute::About
            } else if lower
                .windows(5)
                .any(|part| part.eq_ignore_ascii_case(b"clock"))
            {
                AppRoute::Clock
            } else {
                AppRoute::Terminal
            }
        }
        route => route,
    };
    icon_for_route(route)
}

pub trait LatticeStyle {
    fn spec(&self) -> &StyleSpec;

    /// Layout and hit testing are style policy, while state transitions stay
    /// in `Desktop` and `WindowManager`.
    fn layout_work_area(&self, screen: Rect) -> Rect {
        let top = if crate::top_panel::is_top_panel_enabled() {
            self.metrics().top_panel_height
        } else {
            0
        };
        let bottom = self.metrics().taskbar_height;
        Rect {
            x: screen.x,
            y: screen.y + top as i32,
            width: screen.width,
            height: screen.height.saturating_sub(top).saturating_sub(bottom),
        }
    }

    fn hit_test_chrome(&self, window: &Window, point: Point) -> ChromeHit {
        if window.hit_close_button(point.x, point.y) {
            ChromeHit::Close
        } else if window.hit_minimize_button(point.x, point.y) {
            ChromeHit::Minimize
        } else if window.hit_maximize_button(point.x, point.y) {
            ChromeHit::Maximize
        } else if window.contains_title_bar(point.x, point.y) {
            ChromeHit::Move
        } else {
            ChromeHit::None
        }
    }

    fn metrics(&self) -> &StyleMetrics {
        &self.spec().metrics
    }

    fn palette(&self) -> &Palette {
        &self.spec().palette
    }

    /// Paint only the frame and decorations. The compositor paints the
    /// client surface separately so all styles share the same window model.
    fn draw_window_frame(
        &self,
        canvas: &mut Painter<'_>,
        window: &Window,
        state: WindowVisualState,
    );

    fn draw_menu(&self, canvas: &mut Painter<'_>, menu: &PopupMenu);

    fn draw_taskbar(&self, canvas: &mut Painter<'_>, taskbar: &Taskbar);

    fn draw_top_panel(&self, canvas: &mut Painter<'_>, panel: &TopPanel);
}

impl LatticeStyle for StyleSpec {
    fn spec(&self) -> &StyleSpec {
        self
    }

    fn draw_window_frame(
        &self,
        canvas: &mut Painter<'_>,
        window: &Window,
        state: WindowVisualState,
    ) {
        draw_window_frame(canvas, window, state, self, FrameButtons::Square);
    }

    fn draw_menu(&self, canvas: &mut Painter<'_>, menu: &PopupMenu) {
        draw_menu(canvas, menu, self);
    }

    fn draw_taskbar(&self, canvas: &mut Painter<'_>, taskbar: &Taskbar) {
        draw_basalt_taskbar(canvas, taskbar, self);
    }

    fn draw_top_panel(&self, canvas: &mut Painter<'_>, panel: &TopPanel) {
        draw_top_panel(canvas, panel, self);
    }
}

#[derive(Debug, Clone, Copy)]
pub enum FrameButtons {
    Square,
    Capsule,
    Windows,
}

/// Shared frame geometry. The three styles select different button language,
/// colours, and metrics while keeping this clipping-safe implementation in
/// one place.
pub fn draw_window_frame(
    canvas: &mut Painter<'_>,
    window: &Window,
    state: WindowVisualState,
    spec: &StyleSpec,
    buttons: FrameButtons,
) {
    let Some(title) = window.title.as_deref() else {
        return;
    };

    let border = spec.metrics.window_border as i32;
    let title_h = spec.metrics.title_bar_height;
    let width = window.width.saturating_add(spec.metrics.window_border * 2);
    let height = window
        .height
        .saturating_add(title_h)
        .saturating_add(spec.metrics.window_border * 2);
    let x = window.x - border;
    let y = window.y - border;
    // The square/rounded setting is a shared runtime override; the variant
    // still supplies the default radius when rounded corners are enabled.
    let radius = crate::style::window_radius();
    let palette = &spec.palette;
    let border_color = if state.focused {
        palette.border_active
    } else {
        palette.border_inactive
    };
    let title_color = if state.focused {
        palette.title_active
    } else {
        palette.title_inactive
    };

    canvas.draw_shadow(
        x,
        y,
        width,
        height,
        radius,
        2,
        match buttons {
            FrameButtons::Capsule => 7,
            FrameButtons::Windows => 6,
            FrameButtons::Square => 3,
        },
        palette.window_shadow,
    );
    canvas.rounded_rect(x, y, width, height, radius, border_color);
    canvas.fill_rect(window.x, window.y, window.width, title_h, title_color);
    canvas.fill_rect(
        window.x,
        window.y + title_h as i32,
        window.width,
        window.height,
        palette.surface,
    );

    let separator = if state.focused {
        palette.border_active
    } else {
        palette.border_inactive
    };
    canvas.fill_rect(
        window.x,
        window.y + title_h as i32,
        window.width,
        spec.metrics.window_border,
        separator,
    );

    let close_idx = 0;
    let maximize_idx = if spec.metrics.title_buttons_on_left {
        2
    } else {
        1
    };
    let minimize_idx = if spec.metrics.title_buttons_on_left {
        1
    } else {
        2
    };
    let button_y = window.y + (title_h as i32 - 14) / 2;
    draw_title_button(
        canvas,
        crate::style::title_button_x(window.x, window.width, close_idx),
        button_y,
        palette.danger,
        0,
        buttons,
    );
    draw_title_button(
        canvas,
        crate::style::title_button_x(window.x, window.width, maximize_idx),
        button_y,
        palette.active,
        1,
        buttons,
    );
    draw_title_button(
        canvas,
        crate::style::title_button_x(window.x, window.width, minimize_idx),
        button_y,
        palette.accent,
        2,
        buttons,
    );
    let title_x = crate::style::title_text_x(window.x);
    let title_y = window.y + (title_h as i32 - 14) / 2;
    canvas.draw_text(title_x, title_y, title, palette.text, 14.0);
}

fn draw_title_button(
    canvas: &mut Painter<'_>,
    x: i32,
    y: i32,
    color: u32,
    kind: u32,
    buttons: FrameButtons,
) {
    let radius = match buttons {
        FrameButtons::Square => 2,
        FrameButtons::Capsule => 7,
        FrameButtons::Windows => 4,
    };
    let background = match buttons {
        FrameButtons::Windows if kind != 0 => 0xE7EBF0,
        _ => color,
    };
    let mark = match buttons {
        FrameButtons::Windows if kind != 0 => 0x344054,
        _ => 0xFFFFFF,
    };
    canvas.rounded_rect(x, y, 14, 14, radius, background);
    match kind {
        0 => {
            for offset in 0..6 {
                canvas.set_pixel((x + 4 + offset) as u32, (y + 4 + offset) as u32, mark);
                canvas.set_pixel((x + 9 - offset) as u32, (y + 4 + offset) as u32, mark);
            }
        }
        1 => canvas.fill_rect(x + 4, y + 7, 6, 1, mark),
        _ => {
            canvas.fill_rect(x + 4, y + 4, 6, 1, mark);
            canvas.fill_rect(x + 4, y + 9, 6, 1, mark);
            canvas.fill_rect(x + 4, y + 4, 1, 6, mark);
            canvas.fill_rect(x + 9, y + 4, 1, 6, mark);
        }
    }
}

pub fn draw_menu(canvas: &mut Painter<'_>, menu: &PopupMenu, spec: &StyleSpec) {
    if !menu.visible {
        return;
    }
    let palette = &spec.palette;
    canvas.rounded_rect(
        menu.x as i32,
        menu.y as i32,
        menu.width,
        menu.height,
        8,
        palette.menu_border,
    );
    canvas.rounded_rect(
        menu.x as i32 + 1,
        menu.y as i32 + 1,
        menu.width.saturating_sub(2),
        menu.height.saturating_sub(2),
        7,
        palette.menu_bg,
    );
    for (index, item) in menu.items.iter().enumerate() {
        let y = menu.y + 1 + index as u32 * crate::menu::ITEM_HEIGHT;
        canvas.draw_text(
            menu.x as i32 + 9,
            y as i32 + 4,
            &item.label,
            palette.text,
            13.0,
        );
    }
}

pub fn draw_basalt_taskbar(canvas: &mut Painter<'_>, taskbar: &Taskbar, spec: &StyleSpec) {
    let width = canvas.width;
    let height = canvas.height;
    let bar_h = spec.metrics.taskbar_height;
    let bar_y = height.saturating_sub(bar_h);
    let palette = &spec.palette;
    canvas.fill_rect(0, bar_y as i32, width, bar_h, palette.taskbar_bg);
    let mut x = 4i32;
    for entry in &taskbar.entries {
        let button = if entry.focused {
            palette.taskbar_active_bg
        } else {
            palette.taskbar_inactive_bg
        };
        canvas.fill_rect(x, bar_y as i32 + 3, 120, bar_h.saturating_sub(6), button);
        canvas.draw_text(
            x + 6,
            bar_y as i32 + 7,
            &entry.title,
            palette.taskbar_text,
            13.0,
        );
        x += 124;
    }
    crate::network_menu::render_wifi_icon(
        canvas.fb,
        width,
        height,
        taskbar.wifi_icon_x(width),
        bar_y + 6,
        taskbar.wifi_connected,
        taskbar.wifi_visible,
        taskbar.wifi_signal,
    );
    if let Some((source, message)) = taskbar.debug_msgs.last() {
        let text = if source.is_empty() {
            alloc::format!("[{}]", message)
        } else {
            alloc::format!("{}: {}", source, message)
        };
        canvas.draw_text(x + 4, bar_y as i32 + 7, &text, palette.taskbar_text, 13.0);
    }
    if !taskbar.clock_text.is_empty() {
        canvas.draw_text(
            width.saturating_sub(100) as i32,
            bar_y as i32 + 7,
            &taskbar.clock_text,
            palette.taskbar_text,
            13.0,
        );
    }
}

pub fn draw_top_panel(canvas: &mut Painter<'_>, panel: &TopPanel, spec: &StyleSpec) {
    let height = spec.metrics.top_panel_height;
    if height == 0 {
        return;
    }
    let palette = &spec.palette;
    canvas.fill_rect(0, 0, canvas.width, height, palette.taskbar_bg);
    let label_color = if panel.activities_highlight {
        palette.primary
    } else {
        palette.taskbar_text
    };
    canvas.draw_text(12, 5, "Activities", label_color, 13.0);
    if !panel.clock_text.is_empty() {
        canvas.draw_text(
            canvas.width.saturating_sub(120) as i32,
            5,
            &panel.clock_text,
            palette.taskbar_text,
            13.0,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_routes_and_icons_share_the_same_mapping() {
        assert_eq!(route_for_name("Settings"), AppRoute::Settings);
        assert_eq!(route_for_name("File Mgr"), AppRoute::Files);
        assert_eq!(route_for_name("About Fullerene"), AppRoute::Unknown);
        assert!(core::ptr::eq(
            task_icon("About Fullerene"),
            &crate::icon::ICON_ABOUT
        ));
        assert_eq!(app_grid_route(4), Some(AppRoute::Settings));
        assert_eq!(app_grid_route(7), None);
    }
}
