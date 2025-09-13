// fullerene/flasks/src/part_io.rs
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
}

impl PartitionIo {
    pub fn new(mut file: File, offset: u64, size: u64) -> io::Result<Self> {
        file.seek(SeekFrom::Start(offset))?;
        Ok(Self {
            file,
            offset,
            size,
        })
    }
    
    pub fn into_inner(self) -> io::Result<File> {
        Ok(self.file)
    }
}

impl Read for PartitionIo {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let current_pos = self.file.stream_position()?;
        let partition_pos = current_pos - self.offset;
        let remaining = self.size.saturating_sub(partition_pos);
        if remaining == 0 {
            return Ok(0);
        }
        let bytes_to_read = std::cmp::min(buf.len() as u64, remaining);
        self.file.read(&mut buf[..bytes_to_read as usize])
    }
}

impl Write for PartitionIo {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let current_pos = self.file.stream_position()?;
        let partition_pos = current_pos - self.offset;
        let remaining = self.size.saturating_sub(partition_pos);
        if remaining == 0 {
            return Ok(0);
        }
        let bytes_to_write = std::cmp::min(buf.len() as u64, remaining);
        self.file.write(&buf[..bytes_to_write as usize])
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Seek for PartitionIo {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos_on_partition = match pos {
            SeekFrom::Start(p) => p,
            SeekFrom::End(p) => {
                let new = self.size as i64 + p;
                if new < 0 {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek beyond start"));
                }
                new as u64
            }
            SeekFrom::Current(p) => {
                let current_pos_on_partition = self.file.stream_position()? - self.offset;
                let new = current_pos_on_partition as i64 + p;
                if new < 0 {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek beyond start"));
                }
                new as u64
            }
        };

        if new_pos_on_partition > self.size {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek beyond end"));
        }

        self.file.seek(SeekFrom::Start(self.offset + new_pos_on_partition))?;
        Ok(new_pos_on_partition)
    }
}

/// Copy a file into the FAT filesystem, creating directories as needed
pub fn copy_to_fat<T: Read + Write + Seek>(
    start_dir: &fatfs::Dir<T>,
    src: &Path,
    dest: &str,
) -> io::Result<()> {
    let dest_path = Path::new(dest);
    let mut dir = start_dir.clone();

    // Create intermediate directories
    if let Some(parent) = dest_path.parent() {
        for component in parent.iter() {
            let name = component
                .to_str()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Non-UTF8 path"))?;

            // FATFS is case-insensitive and expects 8.3 names, so normalize here if needed
            dir = dir.open_dir(name).or_else(|_| dir.create_dir(name))?;
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

