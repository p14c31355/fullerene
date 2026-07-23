#![no_std]
extern crate alloc;

pub mod block;
pub mod fat;
pub mod fs;
pub mod io;
pub mod vfs;

pub use block::BlockError;
pub use fs::FsError;
pub use io::{FileReader, Read, Seek, SeekFrom, read_to_end_with_limit};
