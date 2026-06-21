//! Menu actions and info-window dispatch.
//! Extracted from the monolith lib.rs to respect AGENTS.md §10.

use crate::{FB_DIMS, RuntimeState, SOLVENT_CALLBACKS, truncate_to_chars};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use lattice::desktop::DesktopAction;
use lattice::surface::Surface;
use lattice::terminal_surface;
use lattice::terminal_surface::Cell as LatticeCell;
/// Glyph dimensions (from lattice::font).
const GLYPH_W: u32 = 8;
const GLYPH_H: u32 = 16;
/// Default terminal cols/rows for new terminal windows.
const DEFAULT_COLS: u32 = 80;
const DEFAULT_ROWS: u32 = 25;
const TERM_WIN_W: u32 = DEFAULT_COLS * GLYPH_W;
const TERM_WIN_H: u32 = DEFAULT_ROWS * GLYPH_H;

/// Kind of system information window.
#[derive(Clone, Copy)]
pub(crate) enum InfoWindow {
    TaskManager,
    DeviceManager,
    FileManager,
    About,
}

impl InfoWindow {
    fn params(self) -> (&'static str, i32, i32, u32, u32, u32, u32) {
        match self {
            Self::TaskManager => ("Task Manager", 120, 80, 44, 2, 0x0d0d1a, 0xCCCCCC),
            Self::DeviceManager => ("Device Manager", 140, 100, 46, 2, 0x0d1a0d, 0xCCFFCC),
            Self::FileManager => ("File Manager", 160, 120, 50, 3, 0x1a1a0d, 0xFFFFCC),
            Self::About => ("About Fullerene", 180, 140, 32, 0, 0x1a0d1a, 0xFFCCFF),
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
        NewShell => {
            // Defer shell launch — cannot call ensure_terminal_window()
            // or launch_shell() while holding RUNTIME lock (deadlock).
            rt.shell_launch_pending = true;
            rt.desktop.force_full_redraw();
            rt.frame_due = true;
        }
        TaskManager => open_info_window(rt, InfoWindow::TaskManager),
        DeviceManager => open_info_window(rt, InfoWindow::DeviceManager),
        FileManager => open_info_window(rt, InfoWindow::FileManager),
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
            rt.desktop.force_full_redraw();
            rt.frame_due = true;
        }
        OpenEditor => {
            // Defer editor launch — cannot call ensure_editor_window()
            // while holding RUNTIME lock (deadlock).
            rt.editor_launch_pending = true;
            rt.desktop.force_full_redraw();
            rt.frame_due = true;
        }
        SysInfo | Shutdown | Reboot | Separator => {} // TODO
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
            let Some(get_procs) = SOLVENT_CALLBACKS.lock().process_list else {
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
            let Some(get_devs) = SOLVENT_CALLBACKS.lock().device_list else {
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
        InfoWindow::About => String::from(
            "Fullerene OS\n============\n\nA microkernel-based\noperating system\nwritten in Rust.\n\nVersion: 0.1.0\nLicense: MIT/Apache-2.0\n\n(c) 2025-2026\n",
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
            // Defer USB poll to avoid blocking the event loop
            if crate::get_usb_drives().is_empty() {
                rt.usb_poll_pending = true;
            }
            explorer.refresh_sidebar();
            rt.desktop.wm.raise_to_top(id);
            rt.explorer_dirty = true;
            rt.frame_due = true;
        }
        return;
    }

    // Create the explorer window first so the user sees immediate feedback,
    // then conditionally poll USB in the background.  The sidebar is populated
    // from refresh_sidebar() which calls get_usb_drives() — if drives are
    // already available from boot, no poll is needed.
    let win_w: u32 = 640;
    let win_h: u32 = 400;
    let id = rt.desktop.wm.create_titled_window(
        100, 60, win_w, win_h,
        0x1E1E2E, "File Manager",
    );
    let mut explorer = crate::explorer::ExplorerContext::new();
    explorer.window_id = Some(id);

    // Defer USB poll to avoid blocking before window is shown
    if crate::get_usb_drives().is_empty() {
        rt.usb_poll_pending = true;
    }
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

    let cols = 38u32;
    let rows = 9u32;
    let id = rt.desktop.wm.create_titled_window(
        150, 80, cols * GLYPH_W, rows * GLYPH_H,
        0x0d1a1a, "Settings",
    );
    rt.desktop.wm.raise_to_top(id);
    rt.settings_window = Some(id);
    rt.settings_dirty = true;
    rt.desktop.force_full_redraw();
    rt.frame_due = true;
}

/// Open an info window with custom text and coordinates.
fn open_info_window_raw(
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

/// Render a multi-line text string into a Surface. Returns the number of lines rendered.
pub(crate) fn render_text_into_surface(
    surface: &mut Surface,
    text: &str,
    max_cols: u32,
    fg_color: u32,
    bg_color: u32,
) -> u32 {
    let cols = max_cols as usize;
    let lines_count = text.lines().count() as u32;
    let total = cols * lines_count as usize;
    let mut cells: Vec<LatticeCell> = Vec::new();
    cells.resize(
        total,
        LatticeCell {
            ch: b' ',
            fg: fg_color,
            bg: bg_color,
        },
    );

    for (row, line) in text.lines().enumerate() {
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
