// fullerene/flasks/src/part_io.rs
use fatfs::FileSystem;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};

/// A wrapper around a File that limits I/O to a specific partition offset and size.
pub struct PartitionIo<'a> {
    file: &'a mut File,
    offset: u64,
    size: u64,
    current_pos: u64,
}

impl<'a> PartitionIo<'a> {
    pub fn new(file: &'a mut File, offset: u64, size: u64) -> io::Result<Self> {
        Ok(Self {
            file,
            offset,
            size,
            current_pos: 0,
        })
    }
}

impl<'a> Read for PartitionIo<'a> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.size.saturating_sub(self.current_pos);
        let bytes_to_read = std::cmp::min(buf.len() as u64, remaining) as usize;
        if bytes_to_read == 0 {
            return Ok(0);
        }
        self.file
            .seek(SeekFrom::Start(self.offset + self.current_pos))?;
        let read = self.file.read(&mut buf[..bytes_to_read])?;
        self.current_pos += read as u64;
        Ok(read)
    }
}

impl<'a> Write for PartitionIo<'a> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let remaining = self.size.saturating_sub(self.current_pos);
        let bytes_to_write = std::cmp::min(buf.len() as u64, remaining) as usize;
        if bytes_to_write == 0 {
            return Ok(0);
        }
        self.file
            .seek(SeekFrom::Start(self.offset + self.current_pos))?;
        let written = self.file.write(&buf[..bytes_to_write])?;
        self.current_pos += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl<'a> Seek for PartitionIo<'a> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(p) => p,
            SeekFrom::End(p) => (self.size as i64 + p) as u64,
            SeekFrom::Current(p) => (self.current_pos as i64 + p) as u64,
        };
        if new_pos > self.size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek beyond end of partition",
            ));
        }
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
            let found = dir
                .iter()
                .filter_map(|e| e.ok())
                .any(|e| e.file_name().eq_ignore_ascii_case(name));
            dir = if found {
                dir.open_dir(name)?
            } else {
                dir.create_dir(name)?
            };
        }
    }

    // Create and write file
    let mut f = dir.create_file(dest_path.file_name().unwrap().to_str().unwrap())?;
    let mut data = Vec::new();
    let mut src_f = File::open(src)?;
    src_f.read_to_end(&mut data)?;
    f.write_all(&data)?;
    f.flush()?;
    Ok(())
}