//! Universal file viewer: detect kind via Genome, then launch the WASM viewer
//! app for decoding and display.  This module is a thin routing layer; all
//! format-specific logic lives in toluene/viewer/ (the WASM app).

mod document;
mod presentation;
pub mod registry;

use alloc::format;
use alloc::string::String;

pub use document::{BinaryDocument, Document, LaunchTarget, TextDocument};
pub use registry::{DECODERS, DecodeError, Decoder};

use spin::Mutex;

static PENDING_SHELL_COMMAND: Mutex<Option<String>> = Mutex::new(None);

pub fn take_pending_shell_command() -> Option<String> {
    PENDING_SHELL_COMMAND.lock().take()
}

fn request_shell_command(command: String) {
    *PENDING_SHELL_COMMAND.lock() = Some(command);
}

fn request_launch_target(target: LaunchTarget) {
    let command = match target {
        LaunchTarget::Wasm { path, args } => {
            let mut command = format!("wasm {}", path);
            for arg in args {
                command.push(' ');
                command.push_str(&arg);
            }
            command
        }
    };
    request_shell_command(command);

    if let Some(runtime) = crate::RUNTIME_CONTEXT.runtime().as_mut() {
        runtime.shell_launch_pending = true;
        runtime.frame_due = true;
    }
}

/// Check whether the WASM viewer is available in the filesystem.
fn has_wasm_viewer() -> bool {
    crate::RuntimeFile::open("/apps/viewer.wasm").is_ok()
}

pub fn open(path: &str) {
    if !has_wasm_viewer() {
        let mut runtime = crate::RUNTIME_CONTEXT.runtime();
        if let Some(runtime) = runtime.as_mut() {
            show_error_window(
                runtime,
                "Viewer unavailable",
                "The WASM file viewer is not installed.\n\
                 Rebuild the kernel with a working WASM build chain.",
            );
        }
        return;
    }

    let target = LaunchTarget::Wasm {
        path: String::from("/apps/viewer.wasm"),
        args: alloc::vec![String::from(path)],
    };
    request_launch_target(target);
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
