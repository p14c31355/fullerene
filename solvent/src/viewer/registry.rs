//! Minimal decoder registry — all format decoding is now handled by the
//! WASM viewer app (toluene/viewer/).  This module exists only as a
//! fallback when the WASM viewer is not available.

use alloc::string::String;
use alloc::vec::Vec;
use genome::io::{FileReader, SeekFrom, read_to_end_with_limit};

use super::document::{BinaryDocument, Document, LaunchTarget, TextDocument};
use crate::RuntimeFile;

const MAX_DOCUMENT_SIZE: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    Filesystem(genome::FsError),
    Message(String),
    Unsupported,
}

impl From<genome::FsError> for DecodeError {
    fn from(error: genome::FsError) -> Self {
        Self::Filesystem(error)
    }
}

pub trait Decoder: Sync {
    fn probe(&self, _kind: genome::FileKind) -> bool {
        false
    }
    fn open(
        &self,
        reader: &mut dyn FileReader,
        kind: genome::FileKind,
        name: &str,
    ) -> Result<Document, DecodeError>;
}

struct FallbackDecoder;

static FALLBACK: FallbackDecoder = FallbackDecoder;

pub static DECODERS: &[&dyn Decoder] = &[&FALLBACK];

pub fn find(_kind: genome::FileKind) -> &'static dyn Decoder {
    &FALLBACK
}

pub fn decode(path: &str) -> Result<Document, DecodeError> {
    let mut reader = RuntimeFile::open(path).map_err(DecodeError::Filesystem)?;
    let kind = genome::detect(&mut reader, path).map_err(DecodeError::Filesystem)?;
    find(kind).open(&mut reader, kind, path)
}

fn read_data(reader: &mut dyn FileReader) -> Result<Vec<u8>, DecodeError> {
    reader.seek(SeekFrom::Start(0))?;
    read_to_end_with_limit(reader, MAX_DOCUMENT_SIZE).map_err(DecodeError::Filesystem)
}

impl Decoder for FallbackDecoder {
    fn open(
        &self,
        reader: &mut dyn FileReader,
        _kind: genome::FileKind,
        _name: &str,
    ) -> Result<Document, DecodeError> {
        // Last-resort: try UTF-8 text, else show as binary
        let data = read_data(reader)?;
        if let Ok(text) = core::str::from_utf8(&data) {
            Ok(Document::Text(TextDocument {
                text: String::from(text),
            }))
        } else {
            reader.seek(SeekFrom::Start(0))?;
            let size = reader.len()?;
            let mut preview = [0u8; 256];
            let read = reader.read(&mut preview)?;
            Ok(Document::Binary(BinaryDocument {
                size,
                preview: preview[..read].to_vec(),
            }))
        }
    }
}
