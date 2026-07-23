use alloc::string::String;

// ── File system errors ────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    FileNotFound,
    FileExists,
    PermissionDenied,
    InvalidFileDescriptor,
    InvalidSeek,
    DiskFull,
    NotADirectory,
    DirectoryNotEmpty,
    IsADirectory,
    InvalidPath,
    NotSupported,
    InvalidInput,
    UnexpectedEof,
    Io,
}

impl core::fmt::Display for FsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.pad(match self {
            FsError::FileNotFound => "file not found",
            FsError::FileExists => "file already exists",
            FsError::PermissionDenied => "permission denied",
            FsError::InvalidFileDescriptor => "invalid file descriptor",
            FsError::InvalidSeek => "invalid seek",
            FsError::DiskFull => "disk full",
            FsError::NotADirectory => "not a directory",
            FsError::DirectoryNotEmpty => "directory not empty",
            FsError::IsADirectory => "is a directory",
            FsError::InvalidPath => "invalid path",
            FsError::NotSupported => "operation not supported",
            FsError::InvalidInput => "invalid input",
            FsError::UnexpectedEof => "unexpected end of file",
            FsError::Io => "filesystem I/O error",
        })
    }
}

impl From<crate::block::BlockError> for FsError {
    fn from(error: crate::block::BlockError) -> Self {
        match error {
            crate::block::BlockError::BufferTooSmall { .. }
            | crate::block::BlockError::LbaOverflow => Self::InvalidInput,
            crate::block::BlockError::SectorNotFound => Self::FileNotFound,
            crate::block::BlockError::Device => Self::Io,
        }
    }
}

// ── File descriptor ───────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FileDesc {
    pub fd: u32,
    pub ino: u64,
    pub offset: u64,
    pub flags: u32,
}

// ── VNode wrapper ─────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

// ── Package management types ─────────────────────────────────

#[derive(Debug, Clone)]
pub struct PackageEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub binary: String,
    pub runtime: String,
}

pub fn parse_manifest(name: &str, text: &str) -> Option<PackageEntry> {
    let mut version = String::from("0.1.0");
    let mut description = String::new();
    let mut binary = String::from("app.bin");
    let mut runtime = String::from("native");

    for line in text.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            match key {
                "version" => version = String::from(value),
                "description" => description = String::from(value),
                "binary" => binary = String::from(value),
                "runtime" => runtime = String::from(value),
                _ => {}
            }
        }
    }

    Some(PackageEntry {
        name: String::from(name),
        version,
        description,
        binary,
        runtime,
    })
}
