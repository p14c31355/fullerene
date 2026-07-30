//! Universal file viewer.  This module is a thin routing layer — all
//! format-specific decoding lives in the WASM viewer app (toluene/viewer/).

use crate::RuntimeFile;

/// Present text emitted by a non-interactive WASM viewer without requiring a
/// terminal window. This is used for media metadata when no frame can be
/// decoded safely.
pub fn show_text_window(title: &str, text: &str) {
    let mut rt = crate::RUNTIME_CONTEXT.runtime();
    let Some(rt) = rt.as_mut() else { return };
    let cols = 80u32;
    let rows = (text.lines().count() as u32).clamp(3, 40);
    let id = rt
        .desktop
        .wm
        .create_titled_window(100, 60, cols * 8, rows * 16, 0x101018, title);
    if let Some(window) = rt.desktop.wm.windows_mut().iter_mut().find(|w| w.id == id) {
        let _ = crate::menu_actions::render_text_into_surface(
            &mut window.surface,
            text,
            cols,
            0xCCCCFF,
            0x101018,
        );
    }
    rt.desktop.wm.raise_to_top(id);
    rt.frame_due = true;
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
