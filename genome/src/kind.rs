//! Lightweight file-kind detection for stream consumers.
//!
//! Detection only reads a small prefix and restores the reader position, so
//! a decoder can immediately consume the same stream from offset zero.

use crate::FsError;
use crate::io::{Read, Seek, SeekFrom};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Text(TextKind),
    Image(ImageKind),
    Audio(AudioKind),
    Video(VideoKind),
    Archive(ArchiveKind),
    Animation(AnimationKind),
    Application(ApplicationKind),
    Binary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    Plain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Bmp,
    Png,
    Jpeg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioKind {
    Wav,
    Mp3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoKind {
    Mp4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Tar,
    Gzip,
    GzipTar,
    Zip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationKind {
    Rle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationKind {
    Wasm,
}

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

/// Detect a file kind from magic bytes, then fall back to the path extension
/// and finally to UTF-8 text detection. The reader position is preserved.
pub fn detect<R: Read + Seek>(reader: &mut R, path: &str) -> Result<FileKind, FsError> {
    let position = reader.seek(SeekFrom::Current(0))?;
    let mut prefix = [0u8; 512];
    let length = reader.read(&mut prefix)?;
    reader.seek(SeekFrom::Start(position))?;
    let prefix = &prefix[..length];
    let extension = extension(path);

    if prefix.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok(FileKind::Image(ImageKind::Png));
    }
    if prefix.starts_with(b"\xff\xd8\xff") {
        return Ok(FileKind::Image(ImageKind::Jpeg));
    }
    if prefix.starts_with(b"BM") {
        return Ok(FileKind::Image(ImageKind::Bmp));
    }
    if prefix.len() >= 12 && &prefix[..4] == b"RIFF" && &prefix[8..12] == b"WAVE" {
        return Ok(FileKind::Audio(AudioKind::Wav));
    }
    if prefix.len() >= 12 && &prefix[4..8] == b"ftyp" {
        return Ok(FileKind::Video(VideoKind::Mp4));
    }
    if prefix.starts_with(b"PK\x03\x04") || prefix.starts_with(b"PK\x05\x06") {
        return Ok(FileKind::Archive(ArchiveKind::Zip));
    }
    if prefix.starts_with(b"\x1f\x8b") {
        return Ok(FileKind::Archive(if extension == "tgz" {
            ArchiveKind::GzipTar
        } else {
            ArchiveKind::Gzip
        }));
    }
    if prefix.starts_with(b"BARL") {
        return Ok(FileKind::Animation(AnimationKind::Rle));
    }
    if prefix.starts_with(b"\0asm") {
        return Ok(FileKind::Application(ApplicationKind::Wasm));
    }
    if prefix.len() >= 262 && &prefix[257..262] == b"ustar" {
        return Ok(FileKind::Archive(ArchiveKind::Tar));
    }

    let by_extension = match extension {
        "bmp" => Some(FileKind::Image(ImageKind::Bmp)),
        "png" => Some(FileKind::Image(ImageKind::Png)),
        "jpg" | "jpeg" => Some(FileKind::Image(ImageKind::Jpeg)),
        "wav" => Some(FileKind::Audio(AudioKind::Wav)),
        "mp3" => Some(FileKind::Audio(AudioKind::Mp3)),
        "mp4" => Some(FileKind::Video(VideoKind::Mp4)),
        "rle" => Some(FileKind::Animation(AnimationKind::Rle)),
        "wasm" => Some(FileKind::Application(ApplicationKind::Wasm)),
        "tar" => Some(FileKind::Archive(ArchiveKind::Tar)),
        "tgz" => Some(FileKind::Archive(ArchiveKind::GzipTar)),
        "gz" => Some(FileKind::Archive(ArchiveKind::Gzip)),
        "zip" => Some(FileKind::Archive(ArchiveKind::Zip)),
        _ if TEXT_EXTENSIONS.contains(&extension) => Some(FileKind::Text(TextKind::Plain)),
        _ => None,
    };
    if let Some(kind) = by_extension {
        return Ok(kind);
    }

    if !prefix.is_empty() && core::str::from_utf8(prefix).is_ok() {
        Ok(FileKind::Text(TextKind::Plain))
    } else {
        Ok(FileKind::Binary)
    }
}

fn extension(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map(|(_, extension)| extension)
        .unwrap_or("")
        .trim()
}

#[cfg(test)]
mod tests {
    use super::{ApplicationKind, FileKind, ImageKind, TextKind, detect};
    use crate::FsError;
    use crate::io::{Read, Seek, SeekFrom};
    use alloc::vec::Vec;

    struct Cursor {
        data: Vec<u8>,
        position: usize,
    }

    impl Read for Cursor {
        fn read(&mut self, buffer: &mut [u8]) -> Result<usize, FsError> {
            let count = buffer
                .len()
                .min(self.data.len().saturating_sub(self.position));
            buffer[..count].copy_from_slice(&self.data[self.position..self.position + count]);
            self.position += count;
            Ok(count)
        }
    }

    impl Seek for Cursor {
        fn seek(&mut self, position: SeekFrom) -> Result<u64, FsError> {
            let next = match position {
                SeekFrom::Start(offset) => {
                    usize::try_from(offset).map_err(|_| FsError::InvalidSeek)?
                }
                SeekFrom::Current(offset) => self
                    .position
                    .checked_add_signed(offset.try_into().map_err(|_| FsError::InvalidSeek)?)
                    .ok_or(FsError::InvalidSeek)?,
                SeekFrom::End(offset) => self
                    .data
                    .len()
                    .checked_add_signed(offset.try_into().map_err(|_| FsError::InvalidSeek)?)
                    .ok_or(FsError::InvalidSeek)?,
            };
            self.position = next;
            Ok(next as u64)
        }
    }

    #[test]
    fn detects_magic_and_restores_position() {
        let mut reader = Cursor {
            data: b"\x89PNG\r\n\x1a\nrest".to_vec(),
            position: 0,
        };
        assert_eq!(
            detect(&mut reader, "image.bin"),
            Ok(FileKind::Image(ImageKind::Png))
        );
        assert_eq!(reader.position, 0);
    }

    #[test]
    fn uses_text_extension_for_empty_files() {
        let mut reader = Cursor {
            data: Vec::new(),
            position: 0,
        };
        assert_eq!(
            detect(&mut reader, "notes.md"),
            Ok(FileKind::Text(TextKind::Plain))
        );
    }

    #[test]
    fn detects_wasm_magic_and_extension() {
        let mut reader = Cursor {
            data: b"\0asm\x01\0\0\0".to_vec(),
            position: 0,
        };
        assert_eq!(
            detect(&mut reader, "program.bin"),
            Ok(FileKind::Application(ApplicationKind::Wasm))
        );

        let mut empty = Cursor {
            data: Vec::new(),
            position: 0,
        };
        assert_eq!(
            detect(&mut empty, "program.wasm"),
            Ok(FileKind::Application(ApplicationKind::Wasm))
        );
    }
}
