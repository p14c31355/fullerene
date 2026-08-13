//! Menu actions and info-window dispatch.
//! Extracted from the monolith lib.rs to respect AGENTS.md §10.

use crate::{FB_DIMS, RUNTIME_CONTEXT, RuntimeState, network_manager, truncate_to_chars};
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use lattice::desktop::DesktopAction;
use lattice::surface::Surface;
use lattice::terminal_surface;
use lattice::terminal_surface::Cell as LatticeCell;
use spin::Mutex;
/// Glyph dimensions (from lattice::font).
const GLYPH_W: u32 = 8;
const GLYPH_H: u32 = 16;
/// Default terminal cols/rows for new terminal windows.
const DEFAULT_COLS: u32 = 80;
const DEFAULT_ROWS: u32 = 25;
const TERM_WIN_W: u32 = DEFAULT_COLS * GLYPH_W;
const TERM_WIN_H: u32 = DEFAULT_ROWS * GLYPH_H;

const KLOG_MARGIN: u32 = 16;
const KLOG_GAP: u32 = 16;
const KLOG_MIN_W: u32 = 240;
const KLOG_MIN_H: u32 = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowRect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl WindowRect {
    fn right(self) -> i32 {
        self.x.saturating_add(self.width as i32)
    }

    fn bottom(self) -> i32 {
        self.y.saturating_add(self.height as i32)
    }

    fn intersects(self, other: Self) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

fn align_width(width: u32) -> u32 {
    (width / GLYPH_W).max(1) * GLYPH_W
}

fn align_height(height: u32) -> u32 {
    (height / GLYPH_H).max(1) * GLYPH_H
}

fn align_fit(size: u32, unit: u32) -> u32 {
    let aligned = (size / unit) * unit;
    if aligned == 0 { size } else { aligned }
}

/// Choose a Klog Live rectangle that stays inside the work area and avoids
/// the interactive shell whenever there is enough room to show a useful
/// window. The fallback order is: right of shell, left of shell, below shell,
/// then the largest in-bounds rectangle available on a very small display.
fn klog_live_geometry(
    fb_width: u32,
    fb_height: u32,
    work_top: u32,
    work_height: u32,
    shell: Option<WindowRect>,
) -> WindowRect {
    let left = KLOG_MARGIN.min(fb_width.saturating_sub(1));
    let top = work_top.min(fb_height.saturating_sub(1));
    let right = fb_width.saturating_sub(KLOG_MARGIN.min(fb_width));
    let bottom = work_top.saturating_add(work_height).min(fb_height);
    let desired_w = 100 * GLYPH_W;
    let desired_h = 30 * GLYPH_H;
    let max_w = right.saturating_sub(left);
    let max_h = bottom.saturating_sub(top);
    let shell = shell.unwrap_or(WindowRect {
        x: left as i32,
        y: top as i32,
        width: 0,
        height: 0,
    });

    let candidates = [
        (
            shell.right().saturating_add(KLOG_GAP as i32),
            top as i32,
            right.saturating_sub((shell.right().max(0) as u32).saturating_add(KLOG_GAP)),
            max_h,
        ),
        (
            left as i32,
            top as i32,
            (shell.x.max(left as i32) as u32).saturating_sub(left.saturating_add(KLOG_GAP)),
            max_h,
        ),
        (
            left as i32,
            shell.bottom().saturating_add(KLOG_GAP as i32),
            max_w,
            bottom.saturating_sub(
                shell
                    .bottom()
                    .max(top as i32)
                    .saturating_add(KLOG_GAP as i32) as u32,
            ),
        ),
    ];

    for (x, y, available_w, available_h) in candidates {
        let width = align_width(available_w.min(desired_w));
        let height = align_height(available_h.min(desired_h));
        if available_w < KLOG_MIN_W || available_h < KLOG_MIN_H {
            continue;
        }
        let rect = WindowRect {
            x,
            y,
            width,
            height,
        };
        if rect.x >= left as i32
            && rect.y >= top as i32
            && rect.right() <= right as i32
            && rect.bottom() <= bottom as i32
            && !rect.intersects(shell)
        {
            return rect;
        }
    }

    // A display smaller than both windows cannot satisfy non-overlap and
    // minimum-size constraints simultaneously. Keep the fallback bounded;
    // the shell remains the interactive window and Klog Live is still usable.
    WindowRect {
        x: left as i32,
        y: top as i32,
        width: align_fit(max_w.min(desired_w), GLYPH_W),
        height: align_fit(max_h.min(desired_h), GLYPH_H),
    }
}

pub(crate) fn layout_klog_live_window(rt: &mut RuntimeState) {
    let Some(id) = rt.klog_live_window else {
        return;
    };
    let (fb_width, fb_height, _) = *FB_DIMS.lock();
    let fb_width = fb_width.max(640);
    let fb_height = fb_height.max(480);
    let shell = rt
        .term_window
        .and_then(|shell_id| rt.desktop.wm.windows().iter().find(|w| w.id == shell_id))
        .map(|window| WindowRect {
            x: window.x,
            y: window.y,
            width: window.decorated_width(),
            height: window.decorated_height(),
        });
    let rect = klog_live_geometry(
        fb_width,
        fb_height,
        rt.desktop.top_panel_offset(),
        rt.desktop.work_area(fb_width, fb_height).1,
        shell,
    );
    let mut resized = false;
    if let Some(window) = rt.desktop.wm.windows_mut().iter_mut().find(|w| w.id == id) {
        if window.width != rect.width || window.height != rect.height {
            window.surface = Surface::new(rect.width, rect.height, 0x0d0d14);
            window.width = rect.width;
            window.height = rect.height;
            resized = true;
        }
        window.x = rect.x;
        window.y = rect.y;
    }
    if resized {
        rt.klog_live_dirty = true;
    }
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub(crate) enum InfoWindow {
    TaskManager,
    DeviceManager,
    FileManager,
    LogViewer,
    KLogLive,
    SystemInfo,
    About,
}

impl InfoWindow {
    fn params(self) -> (&'static str, i32, i32, u32, u32, u32, u32) {
        match self {
            Self::TaskManager => ("Task Manager", 120, 80, 44, 2, 0x0d0d1a, 0xCCCCCC),
            Self::DeviceManager => ("Device Manager", 140, 100, 46, 2, 0x0d1a0d, 0xCCFFCC),
            Self::FileManager => ("File Manager", 160, 120, 50, 3, 0x1a1a0d, 0xFFFFCC),
            Self::LogViewer => ("Log Viewer", 80, 50, 88, 2, 0x101014, 0xD8D8E8),
            Self::KLogLive => ("KLog Live", 60, 40, 100, 2, 0x0d0d14, 0xAADDFF),
            Self::SystemInfo => ("System Info", 140, 90, 52, 2, 0x101820, 0xCCEEFF),
            Self::About => ("About Fullerene", 180, 140, 40, 0, 0x1a0d1a, 0xFFCCFF),
        }
    }
}

/// Dispatch a context-menu or system-menu action to the appropriate handler.
pub(crate) fn dispatch_menu_action(rt: &mut RuntimeState, action: &DesktopAction) {
    use DesktopAction::*;
    match action {
        NewTerminal => {
            let id = rt
                .desktop
                .wm
                .create_titled_window(60, 50, TERM_WIN_W, TERM_WIN_H, 0x000000, "Terminal");
            rt.desktop.wm.raise_to_top(id);
            rt.frame_due = true;
        }
        TaskManager => open_info_window(rt, InfoWindow::TaskManager),
        DeviceManager => open_info_window(rt, InfoWindow::DeviceManager),
        FileManager => open_info_window(rt, InfoWindow::FileManager),
        LogViewer => open_info_window(rt, InfoWindow::LogViewer),
        KLogLive => open_klog_live_window(rt),
        Refresh => {
            rt.desktop.force_full_redraw();
            rt.frame_due = true;
        }
        About => open_info_window(rt, InfoWindow::About),
        ToggleTiling => {
            let (fw, fh, _stride) = *FB_DIMS.lock();
            let (ww, wh) = rt.desktop.work_area(fw, fh);
            rt.desktop.wm.toggle_tiling();
            rt.desktop.wm.retile(ww, wh);
            rt.frame_due = true;
        }
        OpenEditor => {
            // Defer editor launch — cannot call ensure_editor_window()
            // while holding RUNTIME_CONTEXT lock (deadlock).
            rt.editor_launch_pending = true;
            rt.desktop.force_full_redraw();
            rt.frame_due = true;
        }
        SysInfo => open_info_window(rt, InfoWindow::SystemInfo),
        ShowPowerMenu => {
            let (fw, fh, _) = *FB_DIMS.lock();
            rt.desktop.show_power_menu(fw, fh);
            rt.frame_due = true;
        }
        Shutdown | Reboot => {
            if let Some(control) = crate::RUNTIME_CONTEXT.callback_snapshot().power_control {
                control(match action {
                    Shutdown => crate::PowerAction::Shutdown,
                    Reboot => crate::PowerAction::Reboot,
                    _ => unreachable!(),
                });
            }
        }
        Separator => {}
        ChangeWallpaperSettings => {
            let presets = crate::wallpaper_presets();
            let next = match crate::get_wallpaper() {
                crate::WallpaperMode::SolidColor => crate::WallpaperMode::GridPattern,
                crate::WallpaperMode::GridPattern => crate::WallpaperMode::Gradient,
                crate::WallpaperMode::Gradient => {
                    if presets.is_empty() {
                        crate::WallpaperMode::SolidColor
                    } else {
                        crate::WallpaperMode::Preset(0)
                    }
                }
                crate::WallpaperMode::Preset(idx) => {
                    if idx + 1 < presets.len() {
                        crate::WallpaperMode::Preset(idx + 1)
                    } else {
                        crate::WallpaperMode::SolidColor
                    }
                }
            };
            crate::set_wallpaper(next);
            rt.desktop.force_full_redraw();
            rt.frame_due = true;
        }
        _ => {
            // Try network actions
            network_manager::handle_network_action(rt, action);
        }
    }
}

pub(crate) fn open_info_window(rt: &mut RuntimeState, kind: InfoWindow) {
    // FileManager uses interactive explorer window, not text window
    if matches!(kind, InfoWindow::FileManager) {
        open_explorer_window(rt);
        return;
    }
    let text = match kind {
        InfoWindow::TaskManager => {
            let Some(get_procs) = RUNTIME_CONTEXT.callback_snapshot().process_list else {
                return show_text_window(
                    rt,
                    "Task Manager",
                    120,
                    80,
                    44,
                    2,
                    0x0d0d1a,
                    0xCCCCCC,
                    "PID   NAME              STATE\n----  ----------------  --------\n (no process list callback)\n",
                );
            };
            let procs = get_procs();
            let mut s =
                String::from("PID   NAME              STATE\n----  ----------------  --------\n");
            for p in &procs {
                let state = match p.state {
                    crate::ProcessStateKind::Ready => "ready",
                    crate::ProcessStateKind::Running => "running",
                    crate::ProcessStateKind::Blocked => "blocked",
                    crate::ProcessStateKind::Terminated => "term",
                };
                let _ = core::write!(
                    &mut s,
                    " {:<4}  {:<16}  {:<8}\n",
                    p.pid,
                    truncate_to_chars(&p.name, 16),
                    state
                );
            }
            s
        }
        InfoWindow::DeviceManager => {
            let Some(get_devs) = RUNTIME_CONTEXT.callback_snapshot().device_list else {
                return show_text_window(
                    rt,
                    "Device Manager",
                    140,
                    100,
                    46,
                    2,
                    0x0d1a0d,
                    0xCCFFCC,
                    "DEVICE              TYPE        ENABLED\n------------------  ----------  -------\n (no device list callback)\n",
                );
            };
            let devs = get_devs();
            let mut s = String::from(
                "DEVICE              TYPE        ENABLED\n------------------  ----------  -------\n",
            );
            for d in &devs {
                let n = &d.name[..d.name.len().min(18)];
                let t = &d.dev_type[..d.dev_type.len().min(10)];
                let _ = core::write!(
                    &mut s,
                    " {:<18}  {:<10}  {:<7}\n",
                    n,
                    t,
                    if d.enabled { "yes" } else { "no" }
                );
            }
            s
        }
        InfoWindow::FileManager => String::new(),
        InfoWindow::KLogLive => String::new(), // handled via open_klog_live_window
        InfoWindow::LogViewer => RUNTIME_CONTEXT
            .callback_snapshot()
            .kernel_log
            .map(|snapshot| snapshot())
            .unwrap_or_else(|| String::from("(kernel log callback unavailable)\n")),
        InfoWindow::SystemInfo => RUNTIME_CONTEXT
            .callback_snapshot()
            .metrics
            .map(|snapshot| snapshot())
            .unwrap_or_else(|| String::from("(metrics callback unavailable)\n")),
        InfoWindow::About => String::from(
            "FULLERENE OS\n============\n\nA microkernel-based\noperating system\nwritten in Rust.\n\nVersion: 0.1.0\nLicense: MIT/Apache-2.0\n\n(c) 2025-2026\n",
        ),
    };
    let (title, x, y, cols, extra_rows, bg, fg) = kind.params();
    show_text_window(rt, title, x, y, cols, extra_rows, bg, fg, &text);
}

/// Open the interactive explorer file manager window.
fn open_explorer_window(rt: &mut RuntimeState) {
    // If already open, just focus it and refresh sidebar
    if let Some(ref mut explorer) = rt.explorer {
        if let Some(id) = explorer.window_id {
            if rt.desktop.wm.windows().iter().any(|w| w.id == id) {
                explorer.refresh_sidebar();
                rt.desktop.wm.raise_to_top(id);
                rt.explorer_dirty = true;
                rt.frame_due = true;
                return;
            }
        }
        // Window was closed; fall through to create a new one
    }

    // The sidebar is a read-only view of devices already registered in /dev.
    // Controller activation must not run in the window/input path.
    let win_w: u32 = 640;
    let win_h: u32 = 400;
    let id = rt
        .desktop
        .wm
        .create_titled_window(100, 60, win_w, win_h, 0x1E1E2E, "File Manager");
    let mut explorer = crate::explorer::ExplorerContext::new();
    explorer.window_id = Some(id);

    explorer.refresh_sidebar();
    explorer.navigate_to("/");
    {
        let window = rt.desktop.wm.windows_mut().iter_mut().find(|w| w.id == id);
        if let Some(w) = window {
            crate::explorer::render_explorer(&explorer, &mut w.surface);
            rt.desktop.invalidate_window(id);
        }
    }
    rt.explorer = Some(explorer);
    rt.explorer_dirty = true;
    rt.frame_due = true;
}

/// Create a titled window, fill its surface with `text`, raise to top, and schedule a redraw.
fn show_text_window(
    rt: &mut RuntimeState,
    title: &str,
    x: i32,
    y: i32,
    cols: u32,
    extra_rows: u32,
    bg: u32,
    fg: u32,
    text: &str,
) {
    let rows = (text.lines().count() as u32) + extra_rows;
    let id = rt
        .desktop
        .wm
        .create_titled_window(x, y, cols * GLYPH_W, rows * GLYPH_H, bg, title);
    if let Some(w) = rt.desktop.wm.windows_mut().iter_mut().find(|w| w.id == id) {
        let _ = render_text_into_surface(&mut w.surface, text, cols, fg, bg);
    }
    rt.desktop.wm.raise_to_top(id);
    rt.frame_due = true;
}

/// Open an interactive Settings window.
///
/// Stores the window ID in `rt.settings_window` so that
/// `settings_handle_key` can process keyboard input and
/// `render_settings` redraws the UI on changes.
pub(crate) fn open_settings_window(rt: &mut RuntimeState) {
    // If already open, just focus it.
    if let Some(id) = rt.settings_window {
        if rt.desktop.wm.windows().iter().any(|w| w.id == id) {
            rt.desktop.wm.raise_to_top(id);
            rt.settings_dirty = true;
            rt.frame_due = true;
            return;
        }
    }

    let window_width = 620u32;
    let (fb_width, fb_height, _) = *FB_DIMS.lock();
    let fb_width = fb_width.max(640);
    let fb_height = fb_height.max(480);
    let work_top = rt.desktop.top_panel_offset();
    let work_height = rt.desktop.work_area(fb_width, fb_height).1;
    let window_height = 450u32.min(work_height);
    let x = fb_width.saturating_sub(window_width) / 2;
    let y = work_top + work_height.saturating_sub(window_height) / 2;
    let surface_color = lattice::style::current().palette.surface;
    let id = rt.desktop.wm.create_titled_window(
        x as i32,
        y as i32,
        window_width,
        window_height,
        surface_color,
        "Settings",
    );
    rt.desktop.wm.raise_to_top(id);
    rt.settings_window = Some(id);
    rt.settings_dirty = true;
    rt.desktop.force_full_redraw();
    rt.frame_due = true;
}

/// Render a multi-line text string into a Surface. Returns the number of lines rendered.
pub(crate) fn render_text_into_surface(
    surface: &mut Surface,
    text: &str,
    max_cols: u32,
    fg_color: u32,
    bg_color: u32,
) -> u32 {
    static TEXT_CELLS: Mutex<Vec<LatticeCell>> = Mutex::new(Vec::new());
    let cols = max_cols as usize;
    // Do not allocate a cell buffer for the entire file: a text file can be
    // much larger than the fixed-size viewer window.
    let max_rows = (surface.height() / GLYPH_H).max(1) as usize;
    let lines: Vec<&str> = text.lines().take(max_rows).collect();
    let lines_count = lines.len() as u32;
    let total = cols * lines_count as usize;
    let mut cells = TEXT_CELLS.lock();
    cells.resize(
        total,
        LatticeCell {
            ch: b' ',
            fg: fg_color,
            bg: bg_color,
        },
    );

    for (row, line) in lines.iter().enumerate() {
        for (col, ch) in line.bytes().enumerate() {
            if col < cols {
                let idx = row * cols + col;
                if idx < cells.len() {
                    cells[idx] = LatticeCell {
                        ch,
                        fg: fg_color,
                        bg: bg_color,
                    };
                }
            }
        }
    }

    terminal_surface::render(terminal_surface::RenderParams {
        surface,
        cells: &cells,
        cols: cols as u32,
        cursor_col: None,
        cursor_row: None,
        cursor_visible: false,
    });

    lines_count
}

/// Open a live-updating kernel log viewer window.
/// The window content is automatically refreshed by the event loop.
fn publish_klog_live_geometry(window: &lattice::window::Window) {
    crate::runtime_context::publish_klog_live_surface(
        window.x,
        window.y + lattice::style::title_bar_height() as i32,
        window.surface.width(),
        window.surface.height(),
    );
}

pub(crate) fn open_klog_live_window(rt: &mut RuntimeState) {
    if let Some(id) = rt.klog_live_window {
        if rt.desktop.wm.windows().iter().any(|window| window.id == id) {
            layout_klog_live_window(rt);
            if let Some(window) = rt
                .desktop
                .wm
                .windows()
                .iter()
                .find(|window| window.id == id)
            {
                publish_klog_live_geometry(window);
            }
            rt.klog_live_dirty = true;
            rt.frame_due = true;
            rt.desktop.wm.raise_to_top(id);
            return;
        }
        rt.klog_live_window = None;
        crate::runtime_context::clear_klog_live_surface();
    }
    // Start with the desired size; layout_klog_live_window will shrink and
    // place it against the current shell and framebuffer dimensions.
    let id = rt.desktop.wm.create_titled_window(
        60,
        40,
        100 * GLYPH_W,
        30 * GLYPH_H,
        0x0d0d14,
        "KLog Live",
    );
    rt.klog_live_window = Some(id);
    layout_klog_live_window(rt);
    if let Some(window) = rt
        .desktop
        .wm
        .windows()
        .iter()
        .find(|window| window.id == id)
    {
        publish_klog_live_geometry(window);
    }
    rt.klog_live_dirty = true;
    rt.frame_due = true;
    rt.desktop.wm.raise_to_top(id);
}

pub fn render_klog_live(rt: &mut RuntimeState) {
    let Some(id) = rt.klog_live_window else {
        return;
    };
    let window = match rt.desktop.wm.windows_mut().iter_mut().find(|w| w.id == id) {
        Some(w) => w,
        None => {
            rt.klog_live_window = None;
            crate::runtime_context::clear_klog_live_surface();
            return;
        }
    };
    publish_klog_live_geometry(window);
    // Clear the entire surface to prevent stale rows
    window.surface.pixels_mut().fill(0x0d0d14);
    let log = RUNTIME_CONTEXT
        .callback_snapshot()
        .kernel_log
        .map(|snap| snap())
        .unwrap_or_else(|| String::from("(kernel log unavailable)\n"));
    let cols = (window.surface.width() / GLYPH_W).max(1);
    let rows = (window.surface.height() / GLYPH_H).max(2);
    let lines: Vec<&str> = log
        .lines()
        .rev()
        .take(rows.saturating_sub(1) as usize)
        .collect();
    let text = alloc::format!(
        "--- KLog Live (auto-refresh) ---\n{}",
        lines.into_iter().rev().collect::<Vec<_>>().join("\n")
    );
    let _ = render_text_into_surface(&mut window.surface, &text, cols, 0xAADDFF, 0x0d0d14);
    rt.desktop.invalidate_window(id);
    rt.klog_live_dirty = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inside(rect: WindowRect, width: u32, height: u32, top: u32, work_height: u32) -> bool {
        rect.x >= 0
            && rect.y >= top as i32
            && rect.right() <= width as i32
            && rect.bottom() <= height as i32
            && rect.bottom() <= top.saturating_add(work_height) as i32
    }

    #[test]
    fn klog_live_is_to_the_side_of_shell_when_room_exists() {
        let shell = WindowRect {
            x: 40,
            y: 40,
            width: TERM_WIN_W,
            height: TERM_WIN_H,
        };
        let rect = klog_live_geometry(1920, 1080, 0, 1052, Some(shell));
        assert!(inside(rect, 1920, 1080, 0, 1052));
        assert!(!rect.intersects(shell));
        assert!(rect.x >= shell.right() || rect.right() <= shell.x);
    }

    #[test]
    fn klog_live_falls_below_shell_on_narrow_display() {
        let shell = WindowRect {
            x: 16,
            y: 16,
            width: 640,
            height: 400,
        };
        let rect = klog_live_geometry(800, 900, 0, 872, Some(shell));
        assert!(inside(rect, 800, 900, 0, 872));
        assert!(!rect.intersects(shell));
        assert!(rect.y >= shell.bottom() || rect.x >= shell.right());
    }

    #[test]
    fn klog_live_never_leaves_screen_on_small_display() {
        let rect = klog_live_geometry(320, 240, 24, 188, None);
        assert!(inside(rect, 320, 240, 24, 188));
        assert!(rect.width <= 320);
        assert!(rect.height <= 188);
    }
}
