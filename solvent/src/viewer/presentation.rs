//! Mini presentation layer — only used as a last-resort fallback when the
//! WASM viewer is not available.  Shows text or binary hex view.

use alloc::format;
use alloc::string::{String, ToString};

use super::document::Document;

pub fn present(rt: &mut crate::RuntimeState, document: Document, _name: &str, path: &str) {
    match document {
        Document::Text(doc) => present_text(rt, doc.text, path),
        Document::Binary(doc) => present_binary(rt, doc, _name),
        // Launch targets are handled by `viewer::open` before presentation.
        Document::Launch(_) => {}
    }
}

fn present_text(rt: &mut crate::RuntimeState, text: String, path: &str) {
    let id = rt
        .desktop
        .wm
        .create_titled_window(100, 80, 80 * 8, 25 * 16, 0x0a0a1e, "Text Editor");
    if let Some(old_id) = rt.editor_window
        && rt.desktop.wm.windows().iter().any(|w| w.id == old_id)
    {
        rt.desktop.wm.close_window(old_id);
    }
    rt.editor_window = Some(id);
    rt.editor_buf = lattice::editor::EditorBuffer::from_text(&text);
    rt.editor_file_path = Some(path.to_string());
    rt.editor_dirty = true;
    rt.desktop.force_full_redraw();
    rt.frame_due = true;
    rt.explorer_dirty = true;
}

fn present_binary(rt: &mut crate::RuntimeState, doc: super::document::BinaryDocument, name: &str) {
    let mut msg = format!("File: {}\nSize: {} bytes\n\n", name, doc.size);
    for (off, chunk) in doc.preview.chunks(16).enumerate() {
        msg.push_str(&format!("{:08x}: ", off * 16));
        for b in chunk {
            msg.push_str(&format!("{:02x} ", b));
        }
        msg.push('\n');
    }
    crate::viewer::show_error_window(rt, "Hex Viewer", &msg);
}
