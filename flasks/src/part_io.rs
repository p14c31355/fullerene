// fullerene/flasks/src/part_io.rs
use fatfs::FileSystem;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};

/// A wrapper around a File that limits I/O to a specific partition offset and size.
pub struct PartitionIo {
    file: File,
    offset: u64,
    size: u64,
    current_pos: u64,
}

impl PartitionIo {
    pub fn new(mut file: File, offset: u64, size: u64) -> io::Result<Self> {
        file.seek(SeekFrom::Start(offset))?;
        Ok(Self {
            file,
            offset,
            size,
            current_pos: 0,
        })
    }

    pub fn _take_file(self) -> File {
        self.file
    }
}

impl Read for PartitionIo {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.size.saturating_sub(self.current_pos);
        if remaining == 0 {
            return Ok(0);
        }
        let bytes_to_read = std::cmp::min(buf.len() as u64, remaining);
        let bytes_to_read = bytes_to_read.try_into().unwrap_or(buf.len());
        self.file.seek(SeekFrom::Start(self.offset + self.current_pos))?;
        let read = self.file.read(&mut buf[..bytes_to_read])?;
        self.current_pos += read as u64;
        Ok(read)
    }
}

impl Write for PartitionIo {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let remaining = self.size.saturating_sub(self.current_pos);
        if remaining == 0 {
            return Ok(0);
        }
        let bytes_to_write = std::cmp::min(buf.len() as u64, remaining);
        let bytes_to_write = bytes_to_write.try_into().unwrap_or(buf.len());
        self.file.seek(SeekFrom::Start(self.offset + self.current_pos))?;
        let written = self.file.write(&buf[..bytes_to_write])?;
        self.current_pos += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Seek for PartitionIo {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(p) => p,
            SeekFrom::End(p) => {
                let p_i64 = p;
                if p_i64 < 0 && p_i64.unsigned_abs() > self.size {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek beyond start"));
                }
                if p_i64 >= 0 && (self.size + p_i64 as u64) > self.size {
                    self.size // saturate at end
                } else {
                    (self.size as i64 + p_i64) as u64
                }
            }
            SeekFrom::Current(p) => {
                let new = self.current_pos as i64 + p;
                if new < 0 {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek beyond start"));
                }
                if new as u64 > self.size {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek beyond end"));
                }
                new as u64
            }
        };
        self.current_pos = new_pos;
        Ok(self.current_pos)
    }
}

/// Copy a file into the FAT filesystem, creating directories as needed
pub fn copy_to_fat<T: Read + Write + Seek>(
    fs: &FileSystem<T>,
    src: &Path,
    dest: &str,
) -> io::Result<()> {
    let dest_path = Path::new(dest);
    let mut dir = fs.root_dir();

    // Create intermediate directories
    if let Some(parent) = dest_path.parent() {
        for component in parent.iter() {
            let name = component
                .to_str()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Non-UTF8 path"))?;
            dir = if dir.iter().filter_map(|e| e.ok()).any(|e| e.file_name().eq_ignore_ascii_case(name)) {
                dir.open_dir(name)?
            } else {
                dir.create_dir(name)?
            };
        }
    }

    // Create and write file
    let file_name = dest_path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid destination path"))?;
    let mut f = dir.create_file(file_name)?;
    let mut src_file = File::open(src)?;
    io::copy(&mut src_file, &mut f)?;
    Ok(())
}
