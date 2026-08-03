//! Settings bridge — settings UI event handling dispatched from Solvent.
//!
//! Extracted from lib.rs to keep the main module focused on orchestration.

use crate::runtime_context::{MOUSE_SENSITIVITY_MAX_RAW, MOUSE_SENSITIVITY_MIN_RAW};
use crate::{
    DISPLAY_BRIGHTNESS_X100, FB_DIMS, KLOG_SAVE_ENABLED, MOUSE_SENSITIVITY, RUNTIME_CONTEXT,
};
use alloc::string::String;
use lattice::compositor::WINDOW_CORNER_RADIUS;
use lattice::painter::Painter;
use lattice::wallpaper::{self, WallpaperMode};
use resonance::KeyCode;

/// Selected row in the settings UI.
pub(crate) static SETTINGS_SELECTED: spin::Mutex<u32> = spin::Mutex::new(0);

/// Handle a key event when the settings window is focused (public entry point).
pub fn settings_handle_key(scancode: u8, pressed: bool) {
    let mut rt = RUNTIME_CONTEXT.runtime();
    if let Some(ref mut r) = *rt {
        settings_handle_key_inner(r, scancode, pressed);
    }
}

/// Select a settings row and apply a value change when one of its adjustment
/// controls is clicked.
pub(crate) fn settings_handle_mouse(rt: &mut crate::RuntimeState, x: i32, y: i32) -> bool {
    let Some(settings_id) = rt.settings_window else {
        return false;
    };
    let Some(window) = rt.desktop.wm.windows().iter().find(|w| w.id == settings_id) else {
        return false;
    };
    let relative_x = x - window.x;
    let relative_y = y - window.y - lattice::style::title_bar_height() as i32;
    const ROW_Y: i32 = 112;
    const ROW_HEIGHT: i32 = 42;
    if relative_x < 28
        || relative_x >= window.width as i32 - 28
        || relative_y < ROW_Y
        || relative_y >= ROW_Y + ROW_HEIGHT * 7
    {
        return false;
    }
    let row = ((relative_y - ROW_Y) / ROW_HEIGHT) as u32;
    *SETTINGS_SELECTED.lock() = row;
    // The right side of each card is an adjustment control. Clicking the
    // value itself advances it; clicking the left half of the control moves
    // backward. The keyboard Left/Right path uses the same helper.
    if relative_x >= 440 && relative_x < window.width as i32 - 28 {
        adjust_setting(rt, row, relative_x < 516);
    }
    rt.settings_dirty = true;
    rt.frame_due = true;
    true
}

pub(crate) fn settings_handle_key_inner(rt: &mut crate::RuntimeState, scancode: u8, pressed: bool) {
    let key = crate::scancode_to_resonance_keycode(scancode);
    if !pressed {
        return;
    }

    let mut sel = SETTINGS_SELECTED.lock();

    const ROWS: u32 = 7;
    match key {
        KeyCode::Up => {
            *sel = sel.saturating_sub(1).min(ROWS - 1);
        }
        KeyCode::Down => {
            *sel = (*sel + 1).min(ROWS - 1);
        }
        KeyCode::Left | KeyCode::Right => {
            adjust_setting(rt, *sel, key == KeyCode::Left);
        }
        KeyCode::Escape => {
            if let Some(id) = rt.settings_window.take() {
                rt.desktop.wm.close_window(id);
            }
            rt.settings_dirty = false;
            rt.frame_due = true;
            return;
        }
        _ => {}
    }
    drop(sel);
    rt.settings_dirty = true;
    rt.frame_due = true;
}

fn adjust_setting(rt: &mut crate::RuntimeState, row: u32, dec: bool) {
    match row {
        0 => {
            let next = lattice::style::variant().next(!dec);
            lattice::style::set_variant(next);
            let (fw, fh, _) = *FB_DIMS.lock();
            rt.desktop.relayout_maximized_windows(fw, fh);
            rt.desktop.force_full_redraw();
            persist_settings();
        }
        1 => {
            let cur = MOUSE_SENSITIVITY.load(core::sync::atomic::Ordering::Relaxed);
            let new_val = if dec {
                cur.saturating_sub(1).max(MOUSE_SENSITIVITY_MIN_RAW)
            } else {
                cur.saturating_add(1).min(MOUSE_SENSITIVITY_MAX_RAW)
            };
            MOUSE_SENSITIVITY.store(new_val, core::sync::atomic::Ordering::Relaxed);
            persist_settings();
        }
        2 => {
            let cur = DISPLAY_BRIGHTNESS_X100.load(core::sync::atomic::Ordering::Relaxed) as i32;
            let new_val = if dec {
                (cur - 5).max(10)
            } else {
                (cur + 5).min(100)
            };
            DISPLAY_BRIGHTNESS_X100.store(new_val as u32, core::sync::atomic::Ordering::Relaxed);
            rt.desktop.force_full_redraw();
            persist_settings();
        }
        3 => {
            lattice::top_panel::toggle_top_panel();
            let (fw, fh, _) = *FB_DIMS.lock();
            rt.desktop.relayout_maximized_windows(fw, fh);
            rt.desktop.force_full_redraw();
            persist_settings();
        }
        4 => {
            let cur = WINDOW_CORNER_RADIUS.load(core::sync::atomic::Ordering::Relaxed);
            WINDOW_CORNER_RADIUS.store(
                if cur == 0 { 8 } else { 0 },
                core::sync::atomic::Ordering::Relaxed,
            );
            rt.desktop.force_full_redraw();
            persist_settings();
        }
        5 => {
            let modes = wallpaper::wallpaper_modes();
            let cur = wallpaper::get_wallpaper();
            let cur_idx = modes.iter().position(|(_, m)| *m == cur).unwrap_or(0);
            let next_idx = if dec {
                (cur_idx + modes.len() - 1) % modes.len()
            } else {
                (cur_idx + 1) % modes.len()
            };
            wallpaper::set_wallpaper(modes[next_idx].1);
            rt.desktop.force_full_redraw();
            persist_settings();
        }
        6 => {
            let new_val = !KLOG_SAVE_ENABLED.load(core::sync::atomic::Ordering::Relaxed);
            KLOG_SAVE_ENABLED.store(new_val, core::sync::atomic::Ordering::Relaxed);
            persist_settings();
        }
        _ => {}
    }
}

/// Set to `true` to trigger a deferred settings save (outside the runtime lock).
pub(crate) static PERSIST_PENDING: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

fn persist_settings() {
    PERSIST_PENDING.store(true, core::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn render_settings(rt: &mut crate::RuntimeState) {
    let settings_id = match rt.settings_window {
        Some(id) => id,
        None => return,
    };

    let sens = (MOUSE_SENSITIVITY.load(core::sync::atomic::Ordering::Relaxed) as f32) / 6.0;
    let bright = DISPLAY_BRIGHTNESS_X100.load(core::sync::atomic::Ordering::Relaxed);
    let top_panel = lattice::top_panel::is_top_panel_enabled();
    let corner = WINDOW_CORNER_RADIUS.load(core::sync::atomic::Ordering::Relaxed);
    let lattice_variant = lattice::style::variant().name();
    let sel = *SETTINGS_SELECTED.lock();

    let wp_mode = wallpaper::get_wallpaper();
    let wp_name = match wp_mode {
        WallpaperMode::SolidColor => "solid",
        WallpaperMode::GridPattern => "grid",
        WallpaperMode::Gradient => "gradient",
        WallpaperMode::Preset(idx) => wallpaper::wallpaper_presets()
            .get(idx)
            .map_or("?", |p| p.name),
    };

    let klog_save = KLOG_SAVE_ENABLED.load(core::sync::atomic::Ordering::Relaxed);
    let Some(window) = rt
        .desktop
        .wm
        .windows_mut()
        .iter_mut()
        .find(|w| w.id == settings_id)
    else {
        rt.settings_window = None;
        rt.settings_dirty = false;
        return;
    };

    let width = window.surface.width();
    let height = window.surface.height();
    let mut painter = Painter::new(window.surface.pixels_mut(), width, height);
    painter.fill_rect(0, 0, width, height, 0xF5F8FC);
    painter.draw_text(32, 24, "Settings", 0x17324D, 26.0);
    painter.draw_text(32, 58, "Personalize your Fullerene desktop", 0x607080, 15.0);
    painter.draw_text(32, 88, "Appearance & system", 0x2B76B9, 13.0);

    let values = [
        String::from(lattice_variant),
        alloc::format!("{:.2}", sens),
        alloc::format!("{}.{:02}", bright / 100, bright % 100),
        String::from(if top_panel { "On" } else { "Off" }),
        String::from(if corner > 0 { "Rounded" } else { "Square" }),
        String::from(wp_name),
        String::from(if klog_save { "On" } else { "Off" }),
    ];
    let labels = [
        "Shell style",
        "Mouse sensitivity",
        "Display brightness",
        "Top panel",
        "Window corners",
        "Wallpaper",
        "SD kernel-log save",
    ];
    let descriptions = [
        "Choose the visual language used by the shell",
        "Adjust pointer movement speed",
        "Control scanout brightness",
        "Show or hide the desktop top panel",
        "Use rounded or square window corners",
        "Cycle desktop background styles",
        "Save kernel logs to removable storage",
    ];

    for row in 0..7u32 {
        let y = 112 + row as i32 * 42;
        let selected_bg = if row == sel { 0xDCEEFF } else { 0xFFFFFF };
        painter.rounded_rect(28, y, width.saturating_sub(56), 36, 8, selected_bg);
        painter.draw_text(46, y + 5, labels[row as usize], 0x1F3448, 15.0);
        painter.draw_text(46, y + 22, descriptions[row as usize], 0x718191, 11.0);
        painter.rounded_rect(440, y + 5, 152, 26, 13, 0xE8EEF4);
        painter.draw_text(452, y + 9, "‹", 0x2B76B9, 17.0);
        painter.draw_text(483, y + 9, &values[row as usize], 0x1F3448, 13.0);
        painter.draw_text(570, y + 9, "›", 0x2B76B9, 17.0);
    }

    painter.draw_text(
        32,
        420,
        "Click ‹ / › to adjust · Arrow keys also work · Esc closes",
        0x718191,
        12.0,
    );
    rt.desktop.invalidate_window(settings_id);
    rt.settings_dirty = false;
}
