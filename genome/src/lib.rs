#![no_std]
extern crate alloc;

pub mod android_fs;
pub mod android_lp;
pub mod arch;
pub mod block;
pub mod erofs;
pub mod ext4;
pub mod f2fs;
pub mod fat;
pub mod fs;
pub mod gpt;
pub mod io;
pub mod kind;
pub mod vfs;

pub use block::BlockError;
pub use fs::FsError;
pub use io::{FileReader, Read, Seek, SeekFrom, read_to_end_with_limit};
pub use kind::{
    ArchiveKind, AudioKind, Confidence, DefaultRecognizer, DetectionSource, FileIdentification,
    FileKind, FileRecognizer, ImageKind, VideoKind, detect,
};
