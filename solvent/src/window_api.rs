//! Window lifecycle, redraw control, and file-launch integration.

use alloc::format;
use alloc::string::{String, ToString};
use lattice::window::WindowId;

use crate::{
    DEFAULT_COLS, DEFAULT_ROWS, FB_DIMS, GLYPH_H, GLYPH_W, RUNTIME_CONTEXT, RuntimeState,
    TERM_WIN_H, TERM_WIN_W,
};

pub(crate) static RENDERING_SUSPENDED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub fn write_terminal(text: &str) {
    if let Some(runtime) = RUNTIME_CONTEXT.runtime().as_mut() {
        runtime.term_buf.put_str(text);
        runtime.term_dirty = true;
    }
}

pub fn suspend_rendering() {
    RENDERING_SUSPENDED.store(true, core::sync::atomic::Ordering::SeqCst);
}

pub fn resume_rendering() {
    RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
}

pub fn force_desktop_redraw() {
    if RENDERING_SUSPENDED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return;
    }
    if let Some(runtime) = RUNTIME_CONTEXT.runtime().as_mut() {
        runtime.desktop.force_full_redraw();
        runtime.frame_due = true;
    }
    RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
}

pub fn create_window(
    title: impl Into<String>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Option<WindowId> {
    RUNTIME_CONTEXT.runtime().as_mut().map(|runtime| {
        runtime
            .desktop
            .wm
            .create_titled_window(x, y, width, height, 0x000000, title)
    })
}

pub fn with_window_surface<F, R>(id: WindowId, callback: F) -> Option<R>
where
    F: FnOnce(&mut [u32], u32, u32) -> R,
{
    RUNTIME_CONTEXT.runtime().as_mut().and_then(|runtime| {
        let window = runtime
            .desktop
            .wm
            .windows_mut()
            .iter_mut()
            .find(|window| window.id == id)?;
        if window.minimized {
            return None;
        }
        let (width, height) = (window.surface.width(), window.surface.height());
        Some(callback(window.surface.pixels_mut(), width, height))
    })
}

pub fn invalidate_window(id: WindowId) {
    if let Some(runtime) = RUNTIME_CONTEXT.runtime().as_mut() {
        runtime.desktop.invalidate_window(id);
        runtime.frame_due = true;
        runtime.term_dirty = true;
    }
}

pub fn close_window(id: WindowId) -> bool {
    RUNTIME_CONTEXT
        .runtime()
        .as_mut()
        .is_some_and(|runtime| runtime.desktop.wm.close_window(id))
}

pub fn framebuffer_dims() -> (u32, u32) {
    let (width, height, _) = *FB_DIMS.lock();
    (width, height)
}

pub fn ensure_terminal_window() -> Option<WindowId> {
    let mut runtime = RUNTIME_CONTEXT.runtime();
    let runtime = runtime.as_mut()?;
    if let Some(id) = runtime.term_window
        && runtime
            .desktop
            .wm
            .windows()
            .iter()
            .any(|window| window.id == id)
    {
        return Some(id);
    }
    let id = runtime
        .desktop
        .wm
        .create_titled_window(40, 30, TERM_WIN_W, TERM_WIN_H, 0x000000, "Terminal");
    runtime.term_window = Some(id);
    runtime.desktop.force_full_redraw();
    runtime.frame_due = true;
    runtime.term_dirty = true;
    Some(id)
}

pub fn ensure_editor_window() -> Option<WindowId> {
    RUNTIME_CONTEXT
        .runtime()
        .as_mut()
        .and_then(crate::editor_bridge::ensure_editor_window)
}

pub(crate) fn render_explorer(runtime: &mut RuntimeState) {
    let explorer = match runtime.explorer.as_mut() {
        Some(explorer) => explorer,
        None => return,
    };
    let explorer_id = match explorer.window_id {
        Some(id) => id,
        None => return,
    };
    let window = match runtime
        .desktop
        .wm
        .windows_mut()
        .iter_mut()
        .find(|window| window.id == explorer_id)
    {
        Some(window) => window,
        None => {
            runtime.explorer = None;
            runtime.explorer_dirty = false;
            return;
        }
    };
    crate::explorer::render_explorer(explorer, &mut window.surface);
    runtime.desktop.invalidate_window(explorer_id);
    runtime.explorer_dirty = false;
}

pub fn launch_file(runtime: &mut RuntimeState, path: &str) {
    let name = path.rsplit('/').next().unwrap_or(path);
    let extension = crate::explorer::extension_of(name);
    let app = crate::explorer::lookup_association(extension);
    let extension_lower = extension.to_lowercase();
    let is_text = matches!(
        extension_lower.as_str(),
        "txt"
            | "md"
            | "log"
            | "toml"
            | "rs"
            | "c"
            | "h"
            | "py"
            | "js"
            | "json"
            | "xml"
            | "yml"
            | "yaml"
            | "ini"
            | "cfg"
            | "sh"
            | "bat"
            | "env"
            | "gitignore"
            | "lock"
    );

    if is_text {
        let read_file = RUNTIME_CONTEXT.callback_snapshot().vfs_read;
        let file_content = match read_file {
            Some(read) => match read(path) {
                Ok(data) => match core::str::from_utf8(&data) {
                    Ok(text) => text.to_string(),
                    Err(_) => return,
                },
                Err(_) => return,
            },
            None => return,
        };
        let id = runtime.desktop.wm.create_titled_window(
            100,
            80,
            DEFAULT_COLS * GLYPH_W,
            DEFAULT_ROWS * GLYPH_H,
            0x0a0a1e,
            "Text Editor",
        );
        if let Some(old_id) = runtime.editor_window
            && runtime
                .desktop
                .wm
                .windows()
                .iter()
                .any(|window| window.id == old_id)
        {
            runtime.desktop.wm.close_window(old_id);
        }
        runtime.editor_window = Some(id);
        runtime.editor_buf = lattice::editor::EditorBuffer::from_text(&file_content);
        runtime.editor_file_path = Some(path.to_string());
        runtime.editor_dirty = true;
        runtime.desktop.force_full_redraw();
        runtime.frame_due = true;
        runtime.explorer_dirty = true;
        return;
    }

    match extension_lower.as_str() {
        "bmp" => {
            crate::viewers::open_bmp(runtime, path, name);
            return;
        }
        #[cfg(feature = "minipng")]
        "png" => {
            crate::viewers::open_png(runtime, path, name);
            return;
        }
        "wav" => {
            crate::viewers::open_wav(runtime, path, name);
            return;
        }
        #[cfg(feature = "rmp3")]
        "mp3" => {
            crate::viewers::open_mp3(runtime, path, name);
            return;
        }
        #[cfg(feature = "shiguredo_mp4")]
        "mp4" => {
            crate::viewers::open_mp4(runtime, path, name);
            return;
        }
        "tar" | "gz" | "xz" => {
            crate::viewers::open_tar(runtime, path, name);
            return;
        }
        _ => {}
    }

    let app_name = app.unwrap_or("Unknown");
    let message = format!(
        "File: {}\nType: .{}\nApp: {}\n\nOpening {} is not yet implemented.",
        name, extension, app_name, app_name
    );
    let columns = 50;
    let rows = (message.lines().count() as u32) + 3;
    let id = runtime.desktop.wm.create_titled_window(
        200,
        160,
        columns * GLYPH_W,
        rows * GLYPH_H,
        0x1a1a0d,
        "Open File",
    );
    if let Some(window) = runtime
        .desktop
        .wm
        .windows_mut()
        .iter_mut()
        .find(|window| window.id == id)
    {
        let _ = crate::menu_actions::render_text_into_surface(
            &mut window.surface,
            &message,
            columns,
            0xFFFFCC,
            0x1a1a0d,
        );
    }
    runtime.desktop.wm.raise_to_top(id);
    runtime.frame_due = true;
}
