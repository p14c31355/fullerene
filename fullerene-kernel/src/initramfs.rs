//! Initramfs — CPIO `newc` archive extraction into the VFS.
//!
//! This module is the third layer of the storage stack foundation
//! (`block cache → FAT32 → initramfs`).  It unpacks a CPIO archive into
//! the kernel's VFS at boot, providing the initial root filesystem
//! content (e.g. `/bin/busybox`, `/etc/hostname`, `/init`).
//!
//! # Format
//!
//! The [newc format][newc] is the standard Linux initramfs format.
//! Each entry is prefixed by a 110-byte header followed by a NUL-padded
//! filename and a NUL-padded body.  Directories are entries with mode
//! bits set to directory; regular files are entries with mode bits set to file.
//!
//! The archive is terminated by an entry named `TRAILER!!!`.
//!
//! [newc]: https://www.gnu.org/software/cpio/manual/html_node/Portable-ASCII-Format.html

use alloc::string::String;
use alloc::string::ToString;

/// Magic "070701" (with leading zero) for newc format.
const NEWC_MAGIC: &[u8] = b"070701";

/// Parse a hex field of `len` bytes from `data` starting at `offset`.
fn parse_hex(data: &[u8], offset: usize, len: usize) -> Option<u64> {
    let bytes = data.get(offset..offset + len)?;
    let mut v = 0u64;
    for &b in bytes {
        let n = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => return None,
        };
        v = v.checked_mul(16)?.checked_add(n as u64)?;
    }
    Some(v)
}

/// Align to 4-byte boundary.
fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// One CPIO entry header.  Only the fields used by the unpacker are
/// stored; the remaining fields are kept for future use (e.g. setting
/// permissions).
#[allow(dead_code)]
struct CpioHeader {
    name: String,
    mode: u32,
    uid: u32,
    gid: u32,
    nlink: u32,
    mtime: u64,
    filesize: u64,
    devmajor: u32,
    devminor: u32,
    rdevmajor: u32,
    rdevminor: u32,
}

/// Parse a single CPIO entry header + body from `data` at `offset`.
/// Returns `(header, body_offset, next_offset)` or `None` if the archive
/// is malformed.
fn parse_entry(data: &[u8], offset: usize) -> Option<(CpioHeader, usize, usize)> {
    if offset + 110 > data.len() {
        return None;
    }
    let magic = &data[offset..offset + 6];
    if magic != NEWC_MAGIC {
        return None;
    }
    let namesize = parse_hex(data, offset + 94, 8)? as usize;
    let filesize = parse_hex(data, offset + 54, 8)?;
    let mode = parse_hex(data, offset + 14, 8)? as u32;
    let uid = parse_hex(data, offset + 22, 8)? as u32;
    let gid = parse_hex(data, offset + 30, 8)? as u32;
    let nlink = parse_hex(data, offset + 38, 8)? as u32;
    let mtime = parse_hex(data, offset + 46, 8)?;
    let devmajor = parse_hex(data, offset + 62, 8)? as u32;
    let devminor = parse_hex(data, offset + 70, 8)? as u32;
    let rdevmajor = parse_hex(data, offset + 78, 8)? as u32;
    let rdevminor = parse_hex(data, offset + 86, 8)? as u32;

    // Name follows the header and is aligned to 4 bytes.
    let name_start = offset + 110;
    let name_end = name_start + namesize.saturating_sub(1);
    let name_end_aligned = align4(name_start + namesize);
    if name_end > data.len() {
        return None;
    }
    let name = core::str::from_utf8(&data[name_start..name_end])
        .ok()?
        .to_string();

    // Body follows the aligned name and is aligned to 4 bytes.
    let body_start = name_end_aligned;
    let body_end = body_start + filesize as usize;
    let next = align4(body_end);

    Some((
        CpioHeader {
            name,
            mode,
            uid,
            gid,
            nlink,
            mtime,
            filesize,
            devmajor,
            devminor,
            rdevmajor,
            rdevminor,
        },
        body_start,
        next,
    ))
}

/// Unpack a CPIO `newc` archive into the kernel's VFS.
///
/// Creates directories with `crate::vfs::mkdir` and files with
/// `crate::fs::write_entire_file`.  The caller is responsible for
/// ensuring the VFS is initialised before calling this function.
pub fn unpack(archive: &[u8]) -> Result<usize, &'static str> {
    let mut count = 0usize;
    let mut offset = 0usize;

    while offset < archive.len() {
        let _entry_start = offset;
        let Some((header, body_start, next)) = parse_entry(archive, offset) else {
            break;
        };
        if header.name == "TRAILER!!!" {
            break;
        }
        let path = header.name.trim_start_matches('/');
        if path.is_empty() || path.contains("..") {
            // Defensive: skip empty paths and traversal attempts.
            offset = next;
            continue;
        }
        let abs_path = if path.starts_with('/') { path } else { &path };
        let ftype = header.mode & 0o170000;
        let perm = header.mode & 0o7777;
        if ftype == 0o040000 {
            // Directory
            let _ = crate::vfs::mkdir(abs_path);
            count += 1;
        } else if ftype == 0o100000 {
            // Regular file
            let body = &archive[body_start..body_start + header.filesize as usize];
            if let Ok(parent) = parent_of(abs_path) {
                if !parent.is_empty() && !crate::vfs::exists(parent) {
                    let _ = crate::vfs::mkdir(parent);
                }
            }
            if crate::vfs::exists(abs_path) {
                let _ = crate::fs::remove(abs_path);
            }
            if create_file_with_mode(abs_path).is_ok() {
                let _ = write_file_mode(abs_path, body, perm);
            } else {
                let _ = crate::fs::write_entire_file(abs_path, body);
            }
            count += 1;
        } else if ftype == 0o120000 {
            // Symlink — not supported yet; record but skip.
            log::debug!("initramfs: symlink {} skipped", abs_path);
        }
        offset = next;
    }

    log::info!("initramfs: unpacked {} entries", count);
    Ok(count)
}

/// Return the parent directory of `path`, or empty string if root.
fn parent_of(path: &str) -> Result<&str, &'static str> {
    match path.rfind('/') {
        Some(pos) => {
            if pos == 0 {
                Ok("/")
            } else {
                Ok(&path[..pos])
            }
        }
        None => Ok(""),
    }
}

/// Create a file via the VFS API without setting its content.
fn create_file_with_mode(path: &str) -> Result<(), &'static str> {
    let fd = crate::vfs::open(path, 0);
    if fd.is_ok() {
        return Ok(());
    }
    let _fd = crate::vfs::create(path).map_err(|_| "create failed")?;
    Ok(())
}

/// Write `data` to a freshly created file. The mode argument is
/// ignored on tmpfs (MemFileSystem does not support permissions).
fn write_file_mode(_path: &str, _data: &[u8], _mode: u32) -> Result<(), &'static str> {
    crate::fs::write_entire_file(_path, _data).map_err(|_| "write failed")
}
