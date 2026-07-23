//! Universal file viewer: detect, decode through the registry, then present.

mod document;
mod presentation;
pub mod registry;

use alloc::format;
use alloc::string::String;

pub use document::{BinaryDocument, Document, TextDocument};
pub use registry::{DECODERS, DecodeError, Decoder};

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

    let mut runtime = crate::RUNTIME_CONTEXT.runtime();
    if let Some(runtime) = runtime.as_mut() {
        presentation::present(runtime, document, name, path);
    }
}

#[cfg(test)]
mod tests {
    use super::registry::{DECODERS, find};
    use genome::FileKind;

    #[test]
    fn registry_has_a_fallback_decoder() {
        let decoder = DECODERS.last().unwrap();
        assert!(decoder.probe(FileKind::Binary));
        assert!(find(FileKind::Binary).probe(FileKind::Binary));
    }
}
