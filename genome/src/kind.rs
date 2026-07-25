//! Lightweight file-kind detection for stream consumers.
//!
//! Detection only reads a small prefix and restores the reader position, so
//! a decoder can immediately consume the same stream from offset zero.
//!
//! Genome's role is limited to identifying the file kind from path extension
//! and magic bytes — it does NOT know which app should open the file.
//! Association between `FileKind` and a handler app belongs to Solvent.

use crate::FsError;
use crate::io::{Read, Seek, SeekFrom};

// ── FileKind ────────────────────────────────────────────────────

/// High-level category of a file's content.
///
/// Genome does not deeply parse formats; it uses heuristics (extension +
/// magic) to assign one of these categories.  The consumer (Solvent) is
/// responsible for mapping a `FileKind` to an app that can handle it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Text,
    Image(ImageKind),
    Audio(AudioKind),
    Video(VideoKind),
    Archive(ArchiveKind),
    Animation,
    Wasm,
    Executable,
    Directory,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Bmp,
    Png,
    Jpeg,
    Gif,
    WebP,
    Tiff,
    Svg,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioKind {
    Wav,
    Mp3,
    Flac,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoKind {
    Mp4,
    WebM,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Tar,
    Gzip,
    GzipTar,
    Zip,
    Other,
}

// ── Detection metadata ──────────────────────────────────────────

/// How confident the detector is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Extension-based guess only (no magic check).
    Extension,
    /// Magic-byte match (strong signal).
    Magic,
    /// Both extension and magic agree.
    Confirmed,
}

/// Which technique was used to detect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionSource {
    Extension,
    Magic,
    ExtensionAndMagic,
}

/// Result of file identification.
#[derive(Debug, Clone)]
pub struct FileIdentification {
    pub kind: FileKind,
    pub confidence: Confidence,
    pub source: DetectionSource,
}

// ── FileRecognizer trait ────────────────────────────────────────

/// Something that can recognise a file's kind from its name and a header.
pub trait FileRecognizer {
    fn recognize(&self, name: &str, header: &[u8]) -> Option<FileIdentification>;
}

// ── Default Recognizer ──────────────────────────────────────────

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

fn extension(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map(|(_, extension)| extension)
        .unwrap_or("")
        .trim()
}

fn detect_by_magic(prefix: &[u8], ext: &str) -> Option<(FileKind, DetectionSource)> {
    if prefix.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some((FileKind::Image(ImageKind::Png), DetectionSource::Magic));
    }
    if prefix.starts_with(b"\xff\xd8\xff") {
        return Some((FileKind::Image(ImageKind::Jpeg), DetectionSource::Magic));
    }
    if prefix.starts_with(b"BM") {
        return Some((FileKind::Image(ImageKind::Bmp), DetectionSource::Magic));
    }
    if prefix.starts_with(b"GIF87a") || prefix.starts_with(b"GIF89a") {
        return Some((FileKind::Image(ImageKind::Gif), DetectionSource::Magic));
    }
    if prefix.starts_with(b"fLaC") {
        return Some((FileKind::Audio(AudioKind::Flac), DetectionSource::Magic));
    }
    if prefix.starts_with(b"RIFF") && prefix.len() >= 12 && &prefix[8..12] == b"WAVE" {
        return Some((FileKind::Audio(AudioKind::Wav), DetectionSource::Magic));
    }
    if prefix.len() >= 3 && &prefix[..3] == b"ID3" {
        return Some((FileKind::Audio(AudioKind::Mp3), DetectionSource::Magic));
    }
    if prefix.len() >= 12 && &prefix[4..8] == b"ftyp" {
        return Some((FileKind::Video(VideoKind::Mp4), DetectionSource::Magic));
    }
    if prefix.len() >= 4 && &prefix[..4] == b"\x1a\x45\xdf\xa3" {
        // WebM / Matroska EBML header
        return Some((FileKind::Video(VideoKind::WebM), DetectionSource::Magic));
    }
    if prefix.starts_with(b"PK\x03\x04") || prefix.starts_with(b"PK\x05\x06") {
        return Some((FileKind::Archive(ArchiveKind::Zip), DetectionSource::Magic));
    }
    if prefix.starts_with(b"\x1f\x8b") {
        let kind = if ext == "tgz" {
            FileKind::Archive(ArchiveKind::GzipTar)
        } else {
            FileKind::Archive(ArchiveKind::Gzip)
        };
        return Some((kind, DetectionSource::Magic));
    }
    if prefix.starts_with(b"BARL") {
        return Some((FileKind::Animation, DetectionSource::Magic));
    }
    if prefix.starts_with(b"\0asm") {
        return Some((FileKind::Wasm, DetectionSource::Magic));
    }
    if prefix.len() >= 262 && &prefix[257..262] == b"ustar" {
        return Some((FileKind::Archive(ArchiveKind::Tar), DetectionSource::Magic));
    }
    None
}

fn detect_by_ext(ext: &str) -> Option<(FileKind, DetectionSource)> {
    let kind = match ext {
        "bmp" => FileKind::Image(ImageKind::Bmp),
        "png" => FileKind::Image(ImageKind::Png),
        "jpg" | "jpeg" => FileKind::Image(ImageKind::Jpeg),
        "gif" => FileKind::Image(ImageKind::Gif),
        "webp" => FileKind::Image(ImageKind::WebP),
        "tiff" | "tif" => FileKind::Image(ImageKind::Tiff),
        "svg" => FileKind::Image(ImageKind::Svg),
        "wav" => FileKind::Audio(AudioKind::Wav),
        "mp3" => FileKind::Audio(AudioKind::Mp3),
        "flac" => FileKind::Audio(AudioKind::Flac),
        "mp4" => FileKind::Video(VideoKind::Mp4),
        "webm" => FileKind::Video(VideoKind::WebM),
        "rle" => FileKind::Animation,
        "wasm" => FileKind::Wasm,
        "tar" => FileKind::Archive(ArchiveKind::Tar),
        "tgz" => FileKind::Archive(ArchiveKind::GzipTar),
        "gz" => FileKind::Archive(ArchiveKind::Gzip),
        "zip" => FileKind::Archive(ArchiveKind::Zip),
        _ if TEXT_EXTENSIONS.contains(&ext) => FileKind::Text,
        _ => return None,
    };
    Some((kind, DetectionSource::Extension))
}

/// The default recognizer provided by Genome.
pub struct DefaultRecognizer;

impl FileRecognizer for DefaultRecognizer {
    fn recognize(&self, name: &str, header: &[u8]) -> Option<FileIdentification> {
        let ext = extension(name);
        // Magic takes priority when it gives a clear answer.
        if let Some((kind, _)) = detect_by_magic(header, ext) {
            let ext_kind = detect_by_ext(ext);
            let (confidence, source) = match ext_kind {
                Some((ek, _)) if ek == kind => {
                    (Confidence::Confirmed, DetectionSource::ExtensionAndMagic)
                }
                _ => (Confidence::Magic, DetectionSource::Magic),
            };
            return Some(FileIdentification {
                kind,
                confidence,
                source,
            });
        }
        // Fall back to extension.
        if let Some((kind, source)) = detect_by_ext(ext) {
            return Some(FileIdentification {
                kind,
                confidence: Confidence::Extension,
                source,
            });
        }
        // Last resort: is it valid UTF-8 text?
        if !header.is_empty() && core::str::from_utf8(header).is_ok() {
            return Some(FileIdentification {
                kind: FileKind::Text,
                confidence: Confidence::Extension,
                source: DetectionSource::Extension,
            });
        }
        None
    }
}

/// Convenience wrapper: detect a file kind from a reader + path.
/// Uses `DefaultRecognizer`.
pub fn detect<R: Read + Seek>(reader: &mut R, path: &str) -> Result<FileKind, FsError> {
    let position = reader.seek(SeekFrom::Current(0))?;
    let mut prefix = [0u8; 512];
    let length = if reader.read_exact(&mut prefix).is_ok() {
        512
    } else {
        reader.seek(SeekFrom::Start(position))?;
        reader.read(&mut prefix)?
    };
    reader.seek(SeekFrom::Start(position))?;
    let recognizer = DefaultRecognizer;
    match recognizer.recognize(path, &prefix[..length]) {
        Some(id) => Ok(id.kind),
        None => Ok(FileKind::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    // ── FileIdentification tests ─────────────────────────────────

    #[test]
    fn file_identification_round_trips() {
        let id = FileIdentification {
            kind: FileKind::Image(ImageKind::Jpeg),
            confidence: Confidence::Confirmed,
            source: DetectionSource::ExtensionAndMagic,
        };
        assert_eq!(id.kind, FileKind::Image(ImageKind::Jpeg));
        assert_eq!(id.confidence, Confidence::Confirmed);
        assert_eq!(id.source, DetectionSource::ExtensionAndMagic);
    }

    // ── Magic-byte detection ─────────────────────────────────────

    #[test]
    fn detects_png_by_magic() {
        let id = DefaultRecognizer.recognize("image.bin", b"\x89PNG\r\n\x1a\nrest");
        let id = id.unwrap();
        assert_eq!(id.kind, FileKind::Image(ImageKind::Png));
        assert_eq!(id.source, DetectionSource::Magic);
    }

    #[test]
    fn detects_jpeg_by_magic() {
        let id = DefaultRecognizer.recognize("photo.bin", b"\xff\xd8\xff\xe0");
        let id = id.unwrap();
        assert_eq!(id.kind, FileKind::Image(ImageKind::Jpeg));
        assert_eq!(id.source, DetectionSource::Magic);
    }

    #[test]
    fn detects_bmp_by_magic() {
        let id = DefaultRecognizer.recognize("file.bin", b"BM\x06\x00\x00\x00");
        let id = id.unwrap();
        assert_eq!(id.kind, FileKind::Image(ImageKind::Bmp));
        assert_eq!(id.source, DetectionSource::Magic);
    }

    #[test]
    fn detects_gif_by_magic() {
        let id = DefaultRecognizer.recognize("file.bin", b"GIF89a\x00\x00\x00\x00");
        assert_eq!(id.unwrap().kind, FileKind::Image(ImageKind::Gif));
    }

    #[test]
    fn detects_wav_by_magic() {
        let id = DefaultRecognizer.recognize("audio.bin", b"RIFF\x00\x00\x00\x00WAVE");
        assert_eq!(id.unwrap().kind, FileKind::Audio(AudioKind::Wav));
    }

    #[test]
    fn detects_mp4_by_magic() {
        let id = DefaultRecognizer.recognize(
            "video.bin",
            b"\x00\x00\x00\x1cftypmp42\x00\x00\x00\x00mp42mp41",
        );
        assert_eq!(id.unwrap().kind, FileKind::Video(VideoKind::Mp4));
    }

    #[test]
    fn detects_zip_by_magic() {
        let id = DefaultRecognizer.recognize("data.bin", b"PK\x03\x04\x00\x00\x00\x00");
        assert_eq!(id.unwrap().kind, FileKind::Archive(ArchiveKind::Zip));
    }

    #[test]
    fn detects_gzip_by_magic() {
        let id = DefaultRecognizer.recognize("data.bin", b"\x1f\x8b\x08\x00\x00\x00\x00\x00");
        assert_eq!(id.unwrap().kind, FileKind::Archive(ArchiveKind::Gzip));
    }

    #[test]
    fn detects_tar_by_magic() {
        let mut hdr = [0u8; 512];
        hdr[257..262].copy_from_slice(b"ustar");
        let id = DefaultRecognizer.recognize("archive.tar", &hdr);
        assert_eq!(id.unwrap().kind, FileKind::Archive(ArchiveKind::Tar));
    }

    #[test]
    fn detects_wasm_by_magic() {
        let id = DefaultRecognizer.recognize("module.bin", b"\0asm\x01\0\0\0");
        assert_eq!(id.unwrap().kind, FileKind::Wasm);
    }

    #[test]
    fn detects_rle_by_magic() {
        let id = DefaultRecognizer.recognize("anim.bin", b"BARL\x01\0\0\0");
        assert_eq!(id.unwrap().kind, FileKind::Animation);
    }

    // ── Extension-based detection ────────────────────────────────

    #[test]
    fn detects_by_extension_when_empty() {
        let id = DefaultRecognizer.recognize("notes.md", b"");
        assert_eq!(id.unwrap().kind, FileKind::Text);
    }

    #[test]
    fn detects_rust_source_by_extension() {
        let id = DefaultRecognizer.recognize("main.rs", b"");
        assert_eq!(id.unwrap().kind, FileKind::Text);
    }

    #[test]
    fn detects_png_by_extension_fallback() {
        let id = DefaultRecognizer.recognize("image.png", b"");
        assert_eq!(id.unwrap().kind, FileKind::Image(ImageKind::Png));
    }

    #[test]
    fn detects_wasm_by_extension_fallback() {
        let id = DefaultRecognizer.recognize("module.wasm", b"");
        assert_eq!(id.unwrap().kind, FileKind::Wasm);
    }

    // ── Confidence / source tracking ────────────────────────────

    #[test]
    fn extension_only_gets_extension_confidence() {
        let id = DefaultRecognizer.recognize("readme.txt", b"").unwrap();
        assert_eq!(id.confidence, Confidence::Extension);
        assert_eq!(id.source, DetectionSource::Extension);
    }

    #[test]
    fn magic_and_extension_match_gets_confirmed_confidence() {
        let id = DefaultRecognizer
            .recognize("photo.jpg", b"\xff\xd8\xff\xe0")
            .unwrap();
        assert_eq!(id.confidence, Confidence::Confirmed);
        assert_eq!(id.source, DetectionSource::ExtensionAndMagic);
    }

    #[test]
    fn magic_beats_extension_when_they_disagree() {
        // "photo.txt" contains JPEG magic — magic detection takes priority
        let id = DefaultRecognizer
            .recognize("photo.txt", b"\xff\xd8\xff\xe0")
            .unwrap();
        assert_eq!(id.kind, FileKind::Image(ImageKind::Jpeg));
        assert_eq!(id.confidence, Confidence::Magic);
        assert_eq!(id.source, DetectionSource::Magic);
    }

    // ── Unknown / fallback ──────────────────────────────────────

    #[test]
    fn unknown_extension_with_binary_data_returns_none() {
        // \xff\xff\xff\xff is not valid UTF-8, and .bin has no known extension.
        let id = DefaultRecognizer.recognize("data.bin", b"\xff\xff\xff\xff");
        assert!(id.is_none());
    }

    #[test]
    fn unknown_extension_empty_header_returns_none() {
        let id = DefaultRecognizer.recognize("data.bin", b"");
        assert!(id.is_none());
    }

    #[test]
    fn utf8_content_with_unknown_extension_is_text() {
        let id = DefaultRecognizer.recognize("data.bin", b"hello world");
        assert_eq!(id.unwrap().kind, FileKind::Text);
    }

    // ── detect() convenience wrapper ─────────────────────────────

    #[test]
    fn preserves_reader_position() {
        let mut reader = Cursor {
            data: b"\x89PNG\r\n\x1a\nrest".to_vec(),
            position: 0,
        };
        let _ = detect(&mut reader, "image.bin");
        assert_eq!(reader.position, 0);
    }

    #[test]
    fn detect_convenience_returns_kind() {
        let mut reader = Cursor {
            data: b"\xff\xd8\xff\xe0".to_vec(),
            position: 0,
        };
        assert_eq!(
            detect(&mut reader, "photo.jpg"),
            Ok(FileKind::Image(ImageKind::Jpeg))
        );
    }

    #[test]
    fn detect_unknown_format_returns_unknown_kind() {
        // Non-UTF-8 binary data with unknown extension.
        let mut reader = Cursor {
            data: b"\xff\xff\xff\xff".to_vec(),
            position: 0,
        };
        assert_eq!(detect(&mut reader, "data.bin"), Ok(FileKind::Unknown));
    }

    // ── FileKind utility ─────────────────────────────────────────

    #[test]
    fn all_image_kinds_are_distinct() {
        use ImageKind::*;
        assert_ne!(Bmp as u8, Png as u8);
        assert_ne!(Png as u8, Jpeg as u8);
        assert_ne!(Jpeg as u8, Gif as u8);
    }

    #[test]
    fn all_audio_kinds_are_distinct() {
        use AudioKind::*;
        assert_ne!(Wav as u8, Mp3 as u8);
        assert_ne!(Mp3 as u8, Flac as u8);
    }

    #[test]
    fn tgz_extension_gives_gzip_tar_with_magic() {
        let id = DefaultRecognizer
            .recognize("bundle.tgz", b"\x1f\x8b\x08\x00")
            .unwrap();
        assert_eq!(id.kind, FileKind::Archive(ArchiveKind::GzipTar));
    }
}
