//! Universal file viewer.  This module is a thin routing layer — all
//! format-specific decoding lives in the WASM viewer app (toluene/viewer/).

use crate::RuntimeFile;
use crate::runtime_context::TextViewerState;
use alloc::string::String;
use resonance::KeyCode;

const TEXT_VIEWER_COLS: u32 = 100;
const TEXT_VIEWER_ROWS: u32 = 36;

fn wrapped_row_count(text: &str, cols: usize) -> usize {
    text.split('\n')
        .map(|line| line.chars().count().div_ceil(cols.max(1)).max(1))
        .sum()
}

fn visible_text(text: &str, cols: usize, first_row: usize, rows: usize) -> String {
    let mut result = String::new();
    let mut row = 0usize;
    let end_row = first_row.saturating_add(rows);
    for line in text.split('\n') {
        let chars: alloc::vec::Vec<char> = line.chars().collect();
        let chunks = if chars.is_empty() {
            1
        } else {
            chars.len().div_ceil(cols.max(1))
        };
        for chunk in 0..chunks {
            if row >= first_row && row < end_row {
                if !result.is_empty() {
                    result.push('\n');
                }
                let chunk_text: String = chars
                    .iter()
                    .skip(chunk * cols.max(1))
                    .take(cols.max(1))
                    .collect();
                result.push_str(&chunk_text);
            }
            row += 1;
        }
    }
    result
}

fn render_text_viewer(rt: &mut crate::RuntimeState) {
    let Some(viewer) = rt.text_viewer.as_ref() else {
        return;
    };
    let id = viewer.window_id;
    let scroll_row = viewer.scroll_row;
    let text = viewer.text.as_str();
    let Some(window) = rt.desktop.wm.windows_mut().iter_mut().find(|w| w.id == id) else {
        rt.text_viewer = None;
        return;
    };
    let cols = (window.surface.width() / 8).max(1) as usize;
    let rows = (window.surface.height() / 16).max(1) as usize;
    let visible = visible_text(text, cols, scroll_row, rows);
    let _ = crate::menu_actions::render_text_into_surface(
        &mut window.surface,
        &visible,
        cols as u32,
        0xD8E4F2,
        0x101018,
    );
    rt.desktop.invalidate_window(id);
}

/// Present text emitted by a non-interactive WASM viewer without requiring a
/// terminal window. This is used for media metadata when no frame can be
/// decoded safely.
pub fn show_text_window(title: &str, text: &str) {
    let mut rt = crate::RUNTIME_CONTEXT.runtime();
    let Some(rt) = rt.as_mut() else { return };
    if let Some(old) = rt.text_viewer.take() {
        rt.desktop.wm.close_window(old.window_id);
    }
    let id = rt.desktop.wm.create_titled_window(
        64,
        48,
        TEXT_VIEWER_COLS * 8,
        TEXT_VIEWER_ROWS * 16,
        0x101018,
        title,
    );
    rt.text_viewer = Some(TextViewerState {
        window_id: id,
        text: alloc::string::String::from(text),
        scroll_row: 0,
    });
    rt.desktop.wm.raise_to_top(id);
    render_text_viewer(rt);
    rt.frame_due = true;
}

/// Handle navigation for the focused WASM text viewer.  The complete file is
/// retained in `TextViewerState`; only the visible page is rasterised.
pub fn handle_key(scancode: u8, pressed: bool) -> bool {
    let mut runtime = crate::RUNTIME_CONTEXT.runtime();
    let Some(rt) = runtime.as_mut() else {
        return false;
    };
    let Some(viewer) = rt.text_viewer.as_mut() else {
        return false;
    };
    let Some(top) = rt.desktop.wm.windows().last().map(|window| window.id) else {
        return false;
    };
    if top != viewer.window_id {
        return false;
    }
    if !pressed {
        return true;
    }
    let key = crate::scancode_to_resonance_keycode(scancode);
    let (rows, cols) = rt
        .desktop
        .wm
        .windows()
        .iter()
        .find(|window| window.id == viewer.window_id)
        .map(|window| {
            (
                (window.surface.height() / 16).max(1) as usize,
                (window.surface.width() / 8).max(1) as usize,
            )
        })
        .unwrap_or((TEXT_VIEWER_ROWS as usize, TEXT_VIEWER_COLS as usize));
    let total_rows = wrapped_row_count(&viewer.text, cols);
    let max_scroll = total_rows.saturating_sub(rows);
    match key {
        KeyCode::Escape => {
            let id = viewer.window_id;
            rt.text_viewer = None;
            rt.desktop.wm.close_window(id);
        }
        KeyCode::PageUp => viewer.scroll_row = viewer.scroll_row.saturating_sub(rows),
        KeyCode::PageDown => viewer.scroll_row = (viewer.scroll_row + rows).min(max_scroll),
        KeyCode::Home => viewer.scroll_row = 0,
        KeyCode::End => viewer.scroll_row = max_scroll,
        KeyCode::Up => viewer.scroll_row = viewer.scroll_row.saturating_sub(1),
        KeyCode::Down => viewer.scroll_row = (viewer.scroll_row + 1).min(max_scroll),
        _ => return true,
    }
    if rt.text_viewer.is_some() {
        render_text_viewer(rt);
    }
    rt.frame_due = true;
    true
}

/// Check whether the WASM viewer is available in the filesystem.
fn has_wasm_viewer() -> bool {
    RuntimeFile::open("/apps/viewer.wasm").is_ok()
}

pub fn open(path: &str) {
    log::info!("viewer: open path={}", path);
    if !has_wasm_viewer() {
        log::warn!("viewer: /apps/viewer.wasm not found in VFS");
        let mut rt = crate::RUNTIME_CONTEXT.runtime();
        if let Some(rt) = rt.as_mut() {
            show_error_window(
                rt,
                "Viewer unavailable",
                "The WASM file viewer is not installed.\n\
                 Rebuild the kernel with a working WASM build chain.",
            );
        }
        return;
    }

    let Some(run_wasm) = crate::RUNTIME_CONTEXT.callback_snapshot().run_wasm else {
        log::warn!("viewer: kernel has no WASM execution callback installed");
        let mut rt = crate::RUNTIME_CONTEXT.runtime();
        if let Some(rt) = rt.as_mut() {
            show_error_window(
                rt,
                "Viewer unavailable",
                "The kernel has no WASM execution callback installed.",
            );
        }
        return;
    };

    // Schedule the viewer directly through the kernel callback. This keeps
    // file paths intact (including spaces), avoids a shell window, and keeps
    // decoding off the compositor/input task.
    // WASI does not synthesize argv[0] for us. The viewer expects the usual
    // argv layout: program name followed by the file to open.
    log::info!("viewer: invoking run_wasm path={}", path);
    let args = ["/apps/viewer.wasm", path];
    let code = run_wasm("/apps/viewer.wasm", &args);
    log::info!("viewer: run_wasm returned code={}", code);
    if code != 0 {
        let mut rt = crate::RUNTIME_CONTEXT.runtime();
        if let Some(rt) = rt.as_mut() {
            show_error_window(rt, "Viewer failed", "The WASM viewer could not be started.");
        }
    }
}

/// Open a simple text window (used for error messages).
pub(crate) fn show_error_window(rt: &mut crate::RuntimeState, title: &str, msg: &str) {
    let cols = 50u32;
    let rows = (msg.lines().count() as u32).min(40) + 3;
    let id = rt
        .desktop
        .wm
        .create_titled_window(100, 60, cols * 8, rows * 16, 0x1a1a0d, title);
    if let Some(w) = rt.desktop.wm.windows_mut().iter_mut().find(|w| w.id == id) {
        let _ = crate::menu_actions::render_text_into_surface(
            &mut w.surface,
            msg,
            cols,
            0xFFCCCC,
            0x1a1a0d,
        );
    }
    rt.desktop.wm.raise_to_top(id);
    rt.frame_due = true;
}
