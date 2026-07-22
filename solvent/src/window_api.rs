//! Window lifecycle, redraw control, and file-launch integration.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use lattice::window::WindowId;

use crate::{
    DEFAULT_COLS, DEFAULT_ROWS, FB_DIMS, GLYPH_H, GLYPH_W, RUNTIME_CONTEXT, RuntimeState,
    TERM_WIN_H, TERM_WIN_W,
};

pub(crate) static RENDERING_SUSPENDED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Path to launch, set by event handlers that hold the runtime lock.
/// The event loop picks this up after dropping the lock and calls
/// `launch_file` outside the locked section to avoid VFS deadlocks.
pub(crate) static PENDING_LAUNCH: spin::Mutex<Option<alloc::string::String>> =
    spin::Mutex::new(None);

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

const MAX_READ_SIZE: u64 = 16 * 1024 * 1024;
const JPEG_HEADER_PREFIX_LIMIT: usize = 256 * 1024;
const TEXT_EXTENSIONS: &[&str] = &[
    "txt",
    "md",
    "log",
    "toml",
    "rs",
    "c",
    "h",
    "py",
    "js",
    "json",
    "xml",
    "yml",
    "yaml",
    "ini",
    "cfg",
    "conf",
    "sh",
    "bat",
    "env",
    "gitignore",
    "lock",
];
const MEDIA_EXTENSIONS: &[&str] = &[
    "bmp", "png", "jpg", "jpeg", "wav", "mp3", "mp4", "rle", "tar", "tgz", "gz", "zip",
];

fn is_text_ext(ext: &str) -> bool {
    TEXT_EXTENSIONS.contains(&ext)
}
fn is_media_ext(ext: &str) -> bool {
    MEDIA_EXTENSIONS.contains(&ext)
}

fn read_file_with_limit(path: &str) -> Result<Vec<u8>, &'static str> {
    let read = RUNTIME_CONTEXT
        .callback_snapshot()
        .vfs_read
        .ok_or("VFS read callback not available")?;
    let data = read(path).map_err(|_| "Failed to read file")?;
    if data.len() as u64 > MAX_READ_SIZE {
        return Err("File too large for in-memory viewer");
    }
    Ok(data)
}

fn read_file_prefix(path: &str, limit: usize) -> Result<Vec<u8>, &'static str> {
    let read_prefix = RUNTIME_CONTEXT
        .callback_snapshot()
        .vfs_read_prefix
        .ok_or("VFS prefix-read callback not available")?;
    read_prefix(path, limit).map_err(|_| "Failed to read file header")
}

fn show_open_error(msg: &str) {
    let mut runtime = RUNTIME_CONTEXT.runtime();
    if let Some(runtime) = runtime.as_mut() {
        crate::viewers::show_error(runtime, "Cannot open file", msg);
        runtime.frame_due = true;
    }
}

pub fn launch_file(path: &str) {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = crate::explorer::extension_of(name);
    let ext_lower = ext.to_lowercase();

    if is_text_ext(&ext_lower) {
        let data = match read_file_with_limit(path) {
            Ok(d) => d,
            Err(e) => {
                show_open_error(e);
                return;
            }
        };
        let text = match core::str::from_utf8(&data) {
            Ok(t) => t,
            Err(_) => {
                show_open_error("File is not valid UTF-8 text");
                return;
            }
        };
        let mut runtime = RUNTIME_CONTEXT.runtime();
        let Some(runtime) = runtime.as_mut() else {
            return;
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
            && runtime.desktop.wm.windows().iter().any(|w| w.id == old_id)
        {
            runtime.desktop.wm.close_window(old_id);
        }
        runtime.editor_window = Some(id);
        runtime.editor_buf = lattice::editor::EditorBuffer::from_text(text);
        runtime.editor_file_path = Some(path.to_string());
        runtime.editor_dirty = true;
        runtime.desktop.force_full_redraw();
        runtime.frame_due = true;
        runtime.explorer_dirty = true;
        return;
    }

    if !is_media_ext(&ext_lower) {
        let app = crate::explorer::lookup_association(&ext).unwrap_or("Unknown");
        let msg = format!(
            "File: {}\nType: .{}\nApp: {}\n\nOpening {} is not yet implemented.",
            name, ext, app, app
        );
        let mut runtime = RUNTIME_CONTEXT.runtime();
        let Some(runtime) = runtime.as_mut() else {
            return;
        };
        let cols = 50u32;
        let id = runtime.desktop.wm.create_titled_window(
            200,
            160,
            cols * GLYPH_W,
            (msg.lines().count() as u32 + 3) * GLYPH_H,
            0x1a1a0d,
            "Open File",
        );
        if let Some(w) = runtime
            .desktop
            .wm
            .windows_mut()
            .iter_mut()
            .find(|w| w.id == id)
        {
            let _ = crate::menu_actions::render_text_into_surface(
                &mut w.surface,
                &msg,
                cols,
                0xFFFFCC,
                0x1a1a0d,
            );
        }
        runtime.desktop.wm.raise_to_top(id);
        runtime.frame_due = true;
        return;
    }

    #[cfg(feature = "zune-jpeg")]
    if matches!(ext_lower.as_str(), "jpg" | "jpeg") {
        let prefix = match read_file_prefix(path, JPEG_HEADER_PREFIX_LIMIT) {
            Ok(prefix) => prefix,
            Err(e) => {
                show_open_error(e);
                return;
            }
        };
        match crate::viewers::preflight_jpeg_header(&prefix) {
            Ok(Some(_)) => {}
            Ok(None) => {
                show_open_error("JPEG header not found in the first 256 KiB");
                return;
            }
            Err(e) => {
                show_open_error(&e);
                return;
            }
        }
    }

    #[cfg(feature = "shiguredo_mp4")]
    if ext_lower == "mp4" {
        let data = match read_file_with_limit(path) {
            Ok(data) => data,
            Err(e) => {
                show_open_error(e);
                return;
            }
        };
        let mut runtime = RUNTIME_CONTEXT.runtime();
        let Some(runtime) = runtime.as_mut() else {
            return;
        };
        crate::viewers::open_mp4_data(runtime, data, name);
        return;
    }

    // Media files: read data (may be slow on SDXC but VFS runs without runtime lock)
    let file_data = match read_file_with_limit(path) {
        Ok(d) => d,
        Err(e) => {
            show_open_error(e);
            return;
        }
    };

    // JPEG: decode outside runtime lock
    #[cfg(feature = "zune-jpeg")]
    if matches!(ext_lower.as_str(), "jpg" | "jpeg") {
        let decoded = crate::viewers::decode_jpeg(&file_data);
        let mut runtime = RUNTIME_CONTEXT.runtime();
        let Some(runtime) = runtime.as_mut() else {
            return;
        };
        match decoded {
            Ok(d) => crate::viewers::render_jpeg_window(runtime, d, name),
            Err(e) => crate::viewers::show_error(runtime, "JPEG Error", &e),
        }
        return;
    }

    #[cfg(not(feature = "zune-jpeg"))]
    if matches!(ext_lower.as_str(), "jpg" | "jpeg") {
        show_open_error("JPEG support not compiled in (zune-jpeg feature disabled)");
        return;
    }

    if ext_lower == "rle" {
        let mut runtime = RUNTIME_CONTEXT.runtime();
        let Some(runtime) = runtime.as_mut() else {
            return;
        };
        crate::viewers::open_rle_data(runtime, file_data, name);
        return;
    }

    let mut runtime = RUNTIME_CONTEXT.runtime();
    let Some(runtime) = runtime.as_mut() else {
        return;
    };
    match ext_lower.as_str() {
        "bmp" => crate::viewers::open_bmp_data(runtime, &file_data, name),
        #[cfg(feature = "minipng")]
        "png" => crate::viewers::open_png_data(runtime, &file_data, name),
        #[cfg(not(feature = "minipng"))]
        "png" => crate::viewers::show_error(
            runtime,
            "PNG Error",
            "PNG support not compiled in (minipng feature disabled)",
        ),
        "wav" => crate::viewers::open_wav_data(runtime, &file_data, name),
        "mp3" => crate::viewers::open_mp3_data(runtime, &file_data, name),
        "tar" => crate::viewers::open_tar_data(runtime, &file_data, name),
        #[cfg(feature = "gzip")]
        "tgz" | "gz" => {
            crate::viewers::open_gzip_data(runtime, &file_data, name, ext_lower == "tgz")
        }
        "zip" => crate::viewers::open_zip_data(runtime, &file_data, name),
        _ => {}
    }
}
