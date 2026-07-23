//! Decoder registry. Decoders produce documents; presentation is separate.

use alloc::string::String;
use alloc::vec::Vec;
use genome::io::{FileReader, SeekFrom, read_to_end_with_limit};
use genome::{
    AnimationKind, ApplicationKind, ArchiveKind, AudioKind, FileKind, ImageKind, TextKind,
    VideoKind,
};

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
    fn probe(&self, kind: FileKind) -> bool;
    fn open(
        &self,
        reader: &mut dyn FileReader,
        kind: FileKind,
        name: &str,
    ) -> Result<Document, DecodeError>;
}

struct TextDecoder;
struct ImageDecoder;
struct AudioDecoder;
struct VideoDecoder;
struct ArchiveDecoder;
struct AnimationDecoder;
struct ApplicationDecoder;
struct BinaryDecoder;

static TEXT_DECODER: TextDecoder = TextDecoder;
static IMAGE_DECODER: ImageDecoder = ImageDecoder;
static AUDIO_DECODER: AudioDecoder = AudioDecoder;
static VIDEO_DECODER: VideoDecoder = VideoDecoder;
static ARCHIVE_DECODER: ArchiveDecoder = ArchiveDecoder;
static ANIMATION_DECODER: AnimationDecoder = AnimationDecoder;
static APPLICATION_DECODER: ApplicationDecoder = ApplicationDecoder;
static BINARY_DECODER: BinaryDecoder = BinaryDecoder;

pub static DECODERS: &[&dyn Decoder] = &[
    &TEXT_DECODER,
    &IMAGE_DECODER,
    &AUDIO_DECODER,
    &VIDEO_DECODER,
    &ARCHIVE_DECODER,
    &ANIMATION_DECODER,
    &APPLICATION_DECODER,
    &BINARY_DECODER,
];

pub fn find(kind: FileKind) -> &'static dyn Decoder {
    DECODERS
        .iter()
        .copied()
        .find(|decoder| decoder.probe(kind))
        .unwrap_or(&BINARY_DECODER)
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

impl Decoder for TextDecoder {
    fn probe(&self, kind: FileKind) -> bool {
        matches!(kind, FileKind::Text(TextKind::Plain))
    }

    fn open(
        &self,
        reader: &mut dyn FileReader,
        _kind: FileKind,
        _name: &str,
    ) -> Result<Document, DecodeError> {
        let data = read_data(reader)?;
        let text = core::str::from_utf8(&data)
            .map_err(|_| DecodeError::Message(String::from("File is not valid UTF-8 text")))?;
        Ok(Document::Text(TextDocument {
            text: String::from(text),
        }))
    }
}

impl Decoder for ImageDecoder {
    fn probe(&self, kind: FileKind) -> bool {
        matches!(kind, FileKind::Image(_))
    }

    fn open(
        &self,
        reader: &mut dyn FileReader,
        kind: FileKind,
        _name: &str,
    ) -> Result<Document, DecodeError> {
        #[cfg(feature = "zune-jpeg")]
        if matches!(kind, FileKind::Image(ImageKind::Jpeg)) {
            reader.seek(SeekFrom::Start(0))?;
            let decoded =
                crate::viewers::decode_jpeg_reader(reader).map_err(DecodeError::Message)?;
            return Ok(Document::ImagePixels {
                kind: ImageKind::Jpeg,
                width: u32::from(decoded.width),
                height: u32::from(decoded.height),
                pixels: decoded.pixels,
            });
        }
        Ok(Document::Image {
            kind: match kind {
                FileKind::Image(kind) => kind,
                _ => ImageKind::Bmp,
            },
            data: read_data(reader)?,
        })
    }
}

impl Decoder for AudioDecoder {
    fn probe(&self, kind: FileKind) -> bool {
        matches!(kind, FileKind::Audio(_))
    }

    fn open(
        &self,
        reader: &mut dyn FileReader,
        kind: FileKind,
        _name: &str,
    ) -> Result<Document, DecodeError> {
        Ok(Document::Audio {
            kind: match kind {
                FileKind::Audio(kind) => kind,
                _ => AudioKind::Wav,
            },
            data: read_data(reader)?,
        })
    }
}

impl Decoder for VideoDecoder {
    fn probe(&self, kind: FileKind) -> bool {
        matches!(kind, FileKind::Video(VideoKind::Mp4))
    }

    fn open(
        &self,
        reader: &mut dyn FileReader,
        _kind: FileKind,
        _name: &str,
    ) -> Result<Document, DecodeError> {
        Ok(Document::Video {
            kind: VideoKind::Mp4,
            data: read_data(reader)?,
        })
    }
}

impl Decoder for ArchiveDecoder {
    fn probe(&self, kind: FileKind) -> bool {
        matches!(kind, FileKind::Archive(_))
    }

    fn open(
        &self,
        reader: &mut dyn FileReader,
        kind: FileKind,
        _name: &str,
    ) -> Result<Document, DecodeError> {
        Ok(Document::Archive {
            kind: match kind {
                FileKind::Archive(kind) => kind,
                _ => ArchiveKind::Tar,
            },
            data: read_data(reader)?,
        })
    }
}

impl Decoder for AnimationDecoder {
    fn probe(&self, kind: FileKind) -> bool {
        matches!(kind, FileKind::Animation(AnimationKind::Rle))
    }

    fn open(
        &self,
        reader: &mut dyn FileReader,
        _kind: FileKind,
        _name: &str,
    ) -> Result<Document, DecodeError> {
        Ok(Document::Animation {
            kind: AnimationKind::Rle,
            data: read_data(reader)?,
        })
    }
}

impl Decoder for ApplicationDecoder {
    fn probe(&self, kind: FileKind) -> bool {
        matches!(kind, FileKind::Application(_))
    }

    fn open(
        &self,
        _reader: &mut dyn FileReader,
        kind: FileKind,
        path: &str,
    ) -> Result<Document, DecodeError> {
        match kind {
            FileKind::Application(ApplicationKind::Wasm) => {
                Ok(Document::Launch(LaunchTarget::Wasm {
                    path: String::from(path),
                    args: Vec::new(),
                }))
            }
            _ => Err(DecodeError::Unsupported),
        }
    }
}

impl Decoder for BinaryDecoder {
    fn probe(&self, kind: FileKind) -> bool {
        matches!(kind, FileKind::Binary)
    }

    fn open(
        &self,
        reader: &mut dyn FileReader,
        _kind: FileKind,
        _name: &str,
    ) -> Result<Document, DecodeError> {
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
