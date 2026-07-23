//! Small, `no_std` stream primitives shared by filesystem consumers.
//!
//! The traits intentionally mirror the useful part of `std::io` without
//! depending on the standard library.  Filesystems implement the low-level
//! operations; callers can then decode incrementally or opt into a bounded
//! in-memory read explicitly.

use alloc::vec::Vec;

use crate::FsError;

pub trait Read {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError>;

    fn read_exact(&mut self, mut buf: &mut [u8]) -> Result<(), FsError> {
        while !buf.is_empty() {
            let read = self.read(buf)?;
            if read == 0 {
                return Err(FsError::UnexpectedEof);
            }
            buf = &mut buf[read..];
        }
        Ok(())
    }
}

pub trait Seek {
    fn seek(&mut self, position: SeekFrom) -> Result<u64, FsError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekFrom {
    Start(u64),
    End(i64),
    Current(i64),
}

pub trait FileReader: Read + Seek {
    fn len(&mut self) -> Result<u64, FsError>;
}

/// Read a stream into memory while enforcing a hard upper bound.
pub fn read_to_end_with_limit<R: Read + ?Sized>(
    reader: &mut R,
    limit: usize,
) -> Result<Vec<u8>, FsError> {
    let mut output = Vec::new();
    let mut chunk = [0u8; 4096];

    loop {
        if output.len() == limit {
            let mut probe = [0u8; 1];
            let read = reader.read(&mut probe)?;
            if read != 0 {
                return Err(FsError::DiskFull);
            }
            return Ok(output);
        }

        let want = chunk.len().min(limit - output.len());
        let read = reader.read(&mut chunk[..want])?;
        if read == 0 {
            return Ok(output);
        }
        output.extend_from_slice(&chunk[..read]);
    }
}

#[cfg(test)]
mod tests {
    use super::{Read, read_to_end_with_limit};
    use crate::FsError;

    struct Bytes {
        data: &'static [u8],
        offset: usize,
    }

    impl Read for Bytes {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError> {
            let remaining = &self.data[self.offset..];
            let count = remaining.len().min(buf.len());
            buf[..count].copy_from_slice(&remaining[..count]);
            self.offset += count;
            Ok(count)
        }
    }

    #[test]
    fn bounded_read_accepts_exact_limit() {
        let mut reader = Bytes {
            data: b"hello",
            offset: 0,
        };
        assert_eq!(read_to_end_with_limit(&mut reader, 5).unwrap(), b"hello");
    }

    #[test]
    fn bounded_read_rejects_data_after_limit() {
        let mut reader = Bytes {
            data: b"hello!",
            offset: 0,
        };
        assert_eq!(
            read_to_end_with_limit(&mut reader, 5),
            Err(FsError::DiskFull)
        );
    }
}
