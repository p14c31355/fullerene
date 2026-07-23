//! Presentation of decoded, format-independent documents.

use alloc::format;
use alloc::string::{String, ToString};
use genome::{AnimationKind, ArchiveKind, AudioKind, ImageKind, VideoKind};

use super::document::Document;
use crate::{DEFAULT_COLS, DEFAULT_ROWS, GLYPH_H, GLYPH_W, RuntimeState};

pub fn present(runtime: &mut RuntimeState, document: Document, name: &str, path: &str) {
    match document {
        Document::Text(document) => present_text(runtime, document.text, path),
        Document::Image { kind, data } => match kind {
            ImageKind::Bmp => crate::viewers::open_bmp_data(runtime, &data, name),
            #[cfg(feature = "minipng")]
            ImageKind::Png => crate::viewers::open_png_data(runtime, &data, name),
            #[cfg(not(feature = "minipng"))]
            ImageKind::Png => crate::viewers::show_error(
                runtime,
                "PNG Error",
                "PNG support not compiled in (minipng feature disabled)",
            ),
            ImageKind::Jpeg => {
                let _ = data;
                crate::viewers::show_error(
                    runtime,
                    "JPEG Error",
                    "JPEG support not compiled in (zune-jpeg feature disabled)",
                )
            }
        },
        #[cfg(feature = "zune-jpeg")]
        Document::ImagePixels {
            width,
            height,
            pixels,
            ..
        } => crate::viewers::render_rgb_image_window(runtime, width, height, pixels, name),
        Document::Audio { kind, data } => match kind {
            AudioKind::Wav => crate::viewers::open_wav_data(runtime, &data, name),
            AudioKind::Mp3 => crate::viewers::open_mp3_data(runtime, &data, name),
        },
        Document::Video {
            kind: VideoKind::Mp4,
            #[cfg(feature = "shiguredo_mp4")]
            data,
            #[cfg(not(feature = "shiguredo_mp4"))]
                data: _,
        } => {
            #[cfg(feature = "shiguredo_mp4")]
            crate::viewers::open_mp4_data(runtime, data, name);
            #[cfg(not(feature = "shiguredo_mp4"))]
            crate::viewers::show_error(
                runtime,
                "MP4 Error",
                "MP4 support not compiled in (shiguredo_mp4 feature disabled)",
            );
        }
        Document::Archive { kind, data } => match kind {
            ArchiveKind::Tar => crate::viewers::open_tar_data(runtime, &data, name),
            ArchiveKind::Gzip => {
                #[cfg(feature = "gzip")]
                crate::viewers::open_gzip_data(runtime, &data, name, false);
                #[cfg(not(feature = "gzip"))]
                crate::viewers::show_error(runtime, "gzip Error", "gzip support is disabled");
            }
            ArchiveKind::GzipTar => {
                #[cfg(feature = "gzip")]
                crate::viewers::open_gzip_data(runtime, &data, name, true);
                #[cfg(not(feature = "gzip"))]
                crate::viewers::show_error(runtime, "gzip Error", "gzip support is disabled");
            }
            ArchiveKind::Zip => crate::viewers::open_zip_data(runtime, &data, name),
        },
        Document::Animation {
            kind: AnimationKind::Rle,
            data,
        } => crate::viewers::open_rle_data(runtime, data, name),
        // Launch targets are handled by `viewer::open` before presentation.
        Document::Launch(_) => {}
        Document::Binary(document) => present_binary(runtime, document, name),
    }
}

fn present_text(runtime: &mut RuntimeState, text: String, path: &str) {
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
    runtime.editor_buf = lattice::editor::EditorBuffer::from_text(&text);
    runtime.editor_file_path = Some(path.to_string());
    runtime.editor_dirty = true;
    runtime.desktop.force_full_redraw();
    runtime.frame_due = true;
    runtime.explorer_dirty = true;
}

fn present_binary(
    runtime: &mut RuntimeState,
    document: super::document::BinaryDocument,
    name: &str,
) {
    let mut message = format!("File: {}\nSize: {} bytes\n\n", name, document.size);
    for (offset, chunk) in document.preview.chunks(16).enumerate() {
        message.push_str(&format!("{:08x}: ", offset * 16));
        for byte in chunk {
            message.push_str(&format!("{:02x} ", byte));
        }
        message.push('\n');
    }
    crate::viewers::show_text_window(runtime, "Hex Viewer", &message, 70, 0x101018, 0xCCCCFF);
}
