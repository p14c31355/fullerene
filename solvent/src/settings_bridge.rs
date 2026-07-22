//! Settings bridge — settings UI event handling dispatched from Solvent.
//!
//! Extracted from lib.rs to keep the main module focused on orchestration.

use crate::{
    DISPLAY_BRIGHTNESS_X100, FB_DIMS, KLOG_SAVE_ENABLED, MOUSE_SENSITIVITY, RUNTIME_CONTEXT,
};
use alloc::vec;
use lattice::compositor::WINDOW_CORNER_RADIUS;
use lattice::terminal_surface::{self, Cell as LatticeCell};
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

pub(crate) fn settings_handle_key_inner(rt: &mut crate::RuntimeState, scancode: u8, pressed: bool) {
    let key = crate::scancode_to_resonance_keycode(scancode);
    if !pressed {
        return;
    }

    let mut sel = SETTINGS_SELECTED.lock();

    const ROWS: u32 = 6;
    match key {
        KeyCode::Up => {
            *sel = sel.saturating_sub(1).min(ROWS - 1);
        }
        KeyCode::Down => {
            *sel = (*sel + 1).min(ROWS - 1);
        }
        KeyCode::Left | KeyCode::Right => {
            let dec = key == KeyCode::Left;
            match *sel {
                0 => {
                    let cur = (MOUSE_SENSITIVITY.load(core::sync::atomic::Ordering::Relaxed)
                        as f32)
                        / 6.0;
                    let new_val = if dec {
                        (cur - 0.25).max(0.25)
                    } else {
                        (cur + 0.25).min(4.0)
                    };
                    MOUSE_SENSITIVITY.store(
                        (new_val * 6.0) as i16,
                        core::sync::atomic::Ordering::Relaxed,
                    );
                    persist_settings();
                }
                1 => {
                    let cur =
                        DISPLAY_BRIGHTNESS_X100.load(core::sync::atomic::Ordering::Relaxed) as i32;
                    let new_val = if dec {
                        (cur - 5).max(10)
                    } else {
                        (cur + 5).min(100)
                    };
                    DISPLAY_BRIGHTNESS_X100
                        .store(new_val as u32, core::sync::atomic::Ordering::Relaxed);
                    rt.desktop.force_full_redraw();
                    persist_settings();
                }
                2 => {
                    lattice::top_panel::toggle_top_panel();
                    let (fw, fh, _) = *FB_DIMS.lock();
                    rt.desktop.relayout_maximized_windows(fw, fh);
                    rt.desktop.force_full_redraw();
                    persist_settings();
                }
                3 => {
                    let cur = WINDOW_CORNER_RADIUS.load(core::sync::atomic::Ordering::Relaxed);
                    let new_val = if cur == 0 { 8 } else { 0 };
                    WINDOW_CORNER_RADIUS.store(new_val, core::sync::atomic::Ordering::Relaxed);
                    rt.desktop.force_full_redraw();
                    persist_settings();
                }
                4 => {
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
                5 => {
                    let new_val = !KLOG_SAVE_ENABLED.load(core::sync::atomic::Ordering::Relaxed);
                    KLOG_SAVE_ENABLED.store(new_val, core::sync::atomic::Ordering::Relaxed);
                    persist_settings();
                }
                _ => {}
            }
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
    let window = match rt
        .desktop
        .wm
        .windows_mut()
        .iter_mut()
        .find(|w| w.id == settings_id)
    {
        Some(w) => w,
        None => {
            rt.settings_window = None;
            rt.settings_dirty = false;
            return;
        }
    };

    let sens = (MOUSE_SENSITIVITY.load(core::sync::atomic::Ordering::Relaxed) as f32) / 6.0;
    let bright = DISPLAY_BRIGHTNESS_X100.load(core::sync::atomic::Ordering::Relaxed);
    let top_panel = lattice::top_panel::is_top_panel_enabled();
    let corner = WINDOW_CORNER_RADIUS.load(core::sync::atomic::Ordering::Relaxed);
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

    let info = alloc::format!(
        "{}Settings\n\
         \n\
         {}Mouse Sensitivity: {:.2}\n\
         {}Display Brightness: {}.{:02}\n\
         {}Top Panel: {}\n\
         {}Window Corner: {}\n\
         {}Wallpaper: {}\n\
         {}SD Klog Save: {}",
        highlight(sel, 99),
        highlight(sel, 0),
        sens,
        highlight(sel, 1),
        bright / 100,
        bright % 100,
        highlight(sel, 2),
        if top_panel { "ON " } else { "OFF" },
        highlight(sel, 3),
        if corner > 0 { "Rounded" } else { "Square " },
        highlight(sel, 4),
        wp_name,
        highlight(sel, 5),
        if klog_save { "ON " } else { "OFF" },
    );

    let cols = 38u32;
    let total = cols as usize * 11;
    let mut cells = vec![
        LatticeCell {
            ch: b' ',
            fg: 0xCCFFFF,
            bg: 0x0d1a1a
        };
        total
    ];

    for (row, line) in info.lines().enumerate() {
        for (col, ch) in line.bytes().enumerate() {
            if col < cols as usize {
                let idx = row * (cols as usize) + col;
                if idx < total {
                    cells[idx] = LatticeCell {
                        ch,
                        fg: 0xCCFFFF,
                        bg: 0x0d1a1a,
                    };
                }
            }
        }
    }

    terminal_surface::render(terminal_surface::RenderParams {
        surface: &mut window.surface,
        cells: &cells,
        cols,
        cursor_col: None,
        cursor_row: None,
        cursor_visible: false,
    });
    rt.desktop.invalidate_window(settings_id);
    rt.settings_dirty = false;
}

const fn highlight(sel: u32, row: u32) -> &'static str {
    if row == sel { "> " } else { "  " }
}
