//! Format-independent documents produced by decoders.

use alloc::string::String;
use alloc::vec::Vec;
use genome::{AnimationKind, ArchiveKind, AudioKind, ImageKind, VideoKind};

pub enum Document {
    Text(TextDocument),
    Image {
        kind: ImageKind,
        data: Vec<u8>,
    },
    #[cfg(feature = "zune-jpeg")]
    ImagePixels {
        kind: ImageKind,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
    Audio {
        kind: AudioKind,
        data: Vec<u8>,
    },
    Video {
        kind: VideoKind,
        data: Vec<u8>,
    },
    Archive {
        kind: ArchiveKind,
        data: Vec<u8>,
    },
    Animation {
        kind: AnimationKind,
        data: Vec<u8>,
    },
    Launch(LaunchTarget),
    Binary(BinaryDocument),
}

pub enum LaunchTarget {
    Wasm { path: String, args: Vec<String> },
}

pub struct TextDocument {
    pub text: String,
}

pub struct BinaryDocument {
    pub size: u64,
    pub preview: Vec<u8>,
}
