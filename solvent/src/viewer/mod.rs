//! Universal file viewer: detect, decode through the registry, then present.

mod document;
mod presentation;
pub mod registry;

use alloc::format;
use alloc::string::String;
use spin::Mutex;

pub use document::{BinaryDocument, Document, LaunchTarget, TextDocument};
pub use registry::{DECODERS, DecodeError, Decoder};

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

pub fn open(path: &str) {
    let name = path.rsplit('/').next().unwrap_or(path);
    let document = match registry::decode(path) {
        Ok(document) => document,
        Err(error) => {
            let message = match error {
                DecodeError::Filesystem(error) => format!("Cannot read file: {}", error),
                DecodeError::Message(message) => message,
                DecodeError::Unsupported => String::from("No decoder is registered for this file"),
            };
            let mut runtime = crate::RUNTIME_CONTEXT.runtime();
            if let Some(runtime) = runtime.as_mut() {
                crate::viewers::show_error(runtime, "Cannot open file", &message);
                runtime.frame_due = true;
            }
            return;
        }
    };

    if let Document::Launch(target) = document {
        request_launch_target(target);
        return;
    }

    let mut runtime = crate::RUNTIME_CONTEXT.runtime();
    if let Some(runtime) = runtime.as_mut() {
        presentation::present(runtime, document, name, path);
    }
}

#[cfg(test)]
mod tests {
    use super::registry::{DECODERS, find};
    use genome::{ApplicationKind, FileKind};

    #[test]
    fn registry_has_a_fallback_decoder() {
        let decoder = DECODERS.last().unwrap();
        assert!(decoder.probe(FileKind::Binary));
        assert!(find(FileKind::Binary).probe(FileKind::Binary));
    }

    #[test]
    fn registry_routes_wasm_to_an_application_decoder() {
        let kind = FileKind::Application(ApplicationKind::Wasm);
        assert!(find(kind).probe(kind));
    }
}
