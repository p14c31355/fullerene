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
    Io,
}

impl core::fmt::Display for FsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FsError::FileNotFound => write!(f, "file not found"),
            FsError::FileExists => write!(f, "file already exists"),
            FsError::PermissionDenied => write!(f, "permission denied"),
            FsError::InvalidFileDescriptor => write!(f, "invalid file descriptor"),
            FsError::InvalidSeek => write!(f, "invalid seek"),
            FsError::DiskFull => write!(f, "disk full"),
            FsError::NotADirectory => write!(f, "not a directory"),
            FsError::DirectoryNotEmpty => write!(f, "directory not empty"),
            FsError::IsADirectory => write!(f, "is a directory"),
            FsError::InvalidPath => write!(f, "invalid path"),
            FsError::NotSupported => write!(f, "operation not supported"),
            FsError::InvalidInput => write!(f, "invalid input"),
            FsError::Io => write!(f, "filesystem I/O error"),
        }
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
    pub offset: usize,
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
}

pub fn parse_manifest(name: &str, text: &str) -> Option<PackageEntry> {
    let mut version = String::from("0.1.0");
    let mut description = String::new();
    let mut binary = String::from("app.bin");

    for line in text.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            match key {
                "version" => version = String::from(value),
                "description" => description = String::from(value),
                "binary" => binary = String::from(value),
                _ => {}
            }
        }
    }

    Some(PackageEntry {
        name: String::from(name),
        version,
        description,
        binary,
    })
}
