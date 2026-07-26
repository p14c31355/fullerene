use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::fmt::Write as _;

use crate::contexts::vfs;
pub use genome::fs::{DirEntry, FsError, PackageEntry, parse_manifest};
use genome::io::{FileReader, Read, Seek, SeekFrom};
use spin::Mutex;

static STREAM_FILE: Mutex<Option<(String, FileDesc)>> = Mutex::new(None);

fn basename(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit_once('/')
        .map(|(_, name)| name)
        .unwrap_or(path)
}

fn is_dir(path: &str) -> bool {
    vfs::readdir(path).is_ok()
}

pub fn init() {
    vfs::init_vfs();
    log::info!("File system initialized (VFS + tmpfs)");
}

// ── File descriptor wrapper ───────────────────────────────────

#[derive(Debug, Clone)]
pub struct FileDesc {
    pub fd: u32,
    pub ino: u64,
    pub offset: u64,
    pub flags: u32,
}

impl From<genome::vfs::FileDescriptor> for FileDesc {
    fn from(v: genome::vfs::FileDescriptor) -> Self {
        Self {
            fd: v.fd,
            ino: v.ino,
            offset: v.offset,
            flags: v.flags,
        }
    }
}

// ── Public file operations ────────────────────────────────────

pub fn create_file(path: &str, data: &[u8]) -> Result<(), FsError> {
    let fd_info = vfs::create(path)?;
    if !data.is_empty() {
        let mut remaining = data;
        while !remaining.is_empty() {
            match vfs::write(fd_info.fd, remaining) {
                Ok(0) => {
                    let _ = vfs::close(fd_info.fd);
                    return Err(FsError::InvalidInput);
                }
                Ok(n) => remaining = &remaining[n..],
                Err(e) => {
                    let _ = vfs::close(fd_info.fd);
                    return Err(e);
                }
            }
        }
    }
    let _ = vfs::close(fd_info.fd);
    Ok(())
}

/// Replace or append one bounded file chunk without retaining the complete
/// output in a guest/WASI buffer.  This is used by streaming WASM producers.
pub fn write_file_chunk(
    path: &str,
    offset: u64,
    data: &[u8],
    replace: bool,
) -> Result<(), FsError> {
    let mut stream = STREAM_FILE.lock();
    let same_path = stream.as_ref().is_some_and(|(current, _)| current == path);
    if replace || !same_path {
        if let Some((_, old_file)) = stream.take() {
            close_file(old_file)?;
        }
        let descriptor = if replace {
            if vfs::exists(path) {
                vfs::unlink(path)?;
            }
            vfs::create(path)?
        } else {
            match vfs::open(path, 0) {
                Ok(fd) => fd,
                Err(FsError::FileNotFound) => vfs::create(path)?,
                Err(error) => return Err(error),
            }
        };
        stream.replace((path.to_string(), FileDesc::from(descriptor)));
    }

    let file = &mut stream.as_mut().expect("stream file installed").1;
    let result = (|| {
        if file.offset != offset {
            seek_file(file, offset)?;
        }
        let mut remaining = data;
        while !remaining.is_empty() {
            let written = write_file(file, remaining)?;
            if written == 0 {
                return Err(FsError::InvalidInput);
            }
            remaining = &remaining[written..];
        }
        Ok(())
    })();
    result
}

pub fn finish_file_chunk() -> Result<(), FsError> {
    let mut stream = STREAM_FILE.lock();
    if let Some((_, file)) = stream.take() {
        close_file(file)?;
    }
    Ok(())
}

pub fn create_dir(path: &str) -> Result<(), FsError> {
    vfs::mkdir(path)
}

pub fn remove(path: &str) -> Result<(), FsError> {
    vfs::unlink(path)
}

pub fn open_file(path: &str) -> Result<FileDesc, FsError> {
    vfs::open(path, 0).map(FileDesc::from)
}

pub fn close_file(fd: FileDesc) -> Result<(), FsError> {
    vfs::close(fd.fd)
}

pub fn read_file(fd: &mut FileDesc, buffer: &mut [u8]) -> Result<usize, FsError> {
    let n = vfs::read(fd.fd, buffer)?;
    fd.offset = fd
        .offset
        .checked_add(n as u64)
        .ok_or(FsError::InvalidSeek)?;
    Ok(n)
}

pub fn write_file(fd: &mut FileDesc, data: &[u8]) -> Result<usize, FsError> {
    let written = vfs::write(fd.fd, data)?;
    fd.offset = fd
        .offset
        .checked_add(written as u64)
        .ok_or(FsError::InvalidSeek)?;
    Ok(written)
}

pub fn seek_file(fd: &mut FileDesc, position: u64) -> Result<(), FsError> {
    vfs::seek(fd.fd, position).map(|_| {
        fd.offset = position;
    })
}

pub fn file_position(fd: &FileDesc) -> Result<u64, FsError> {
    vfs::position(fd.fd)
}

pub fn file_size_for_handle(fd: &FileDesc) -> Result<u64, FsError> {
    vfs::size(fd.fd)
}

impl Read for FileDesc {
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, FsError> {
        read_file(self, buffer)
    }
}

impl Seek for FileDesc {
    fn seek(&mut self, position: SeekFrom) -> Result<u64, FsError> {
        let offset = match position {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::Current(offset) => self.offset.checked_add_signed(offset),
            SeekFrom::End(offset) => file_size_for_handle(self)?.checked_add_signed(offset),
        }
        .ok_or(FsError::InvalidSeek)?;
        seek_file(self, offset)?;
        Ok(offset)
    }
}

impl FileReader for FileDesc {
    fn len(&mut self) -> Result<u64, FsError> {
        file_size_for_handle(self)
    }
}

pub fn list_dir(path: &str) -> Result<Vec<DirEntry>, FsError> {
    vfs::readdir(path).map(|entries| {
        entries
            .into_iter()
            .map(|v| DirEntry {
                name: v.name,
                size: v.size,
                is_dir: v.is_dir,
            })
            .collect()
    })
}

pub fn exists(path: &str) -> bool {
    vfs::exists(path)
}

pub fn working_directory() -> Result<String, FsError> {
    vfs::working_directory()
}

pub fn change_directory(path: &str) -> Result<(), FsError> {
    vfs::change_directory(path)
}

pub fn copy_file(src: &str, dst: &str) -> Result<(), FsError> {
    let dst = if is_dir(dst) {
        let name = basename(src);
        alloc::format!("{}/{}", dst.trim_end_matches('/'), name)
    } else {
        dst.to_string()
    };
    let data = read_entire_file(src)?;
    write_entire_file(&dst, &data)
}

pub fn move_file(src: &str, dst: &str) -> Result<(), FsError> {
    copy_file(src, dst)?;
    remove(src)
}

pub fn walk_dir(path: &str) -> Result<Vec<String>, FsError> {
    let mut result = Vec::new();
    let entries = list_dir(path)?;
    let prefix = if path.ends_with('/') {
        alloc::format!("{}{}", path, "")
    } else {
        alloc::format!("{}/", path)
    };
    for entry in &entries {
        let full = alloc::format!("{}{}", prefix, entry.name);
        result.push(full.clone());
        if entry.is_dir {
            result.extend(walk_dir(&full)?);
        }
    }
    Ok(result)
}

pub fn read_entire_file(path: &str) -> Result<Vec<u8>, FsError> {
    const MAX_FILE_SIZE: usize = 16 * 1024 * 1024;
    const TIMEOUT_MS: u64 = 15_000;
    const WASM_DIAGNOSTICS: bool = cfg!(debug_assertions);
    let reported_size = file_size(path);
    if WASM_DIAGNOSTICS {
        crate::klog_fmt!(
            "[WASM-DIAG] read_entire begin path={} file_size={:?}\n",
            path,
            reported_size
        );
    }
    if matches!(reported_size, Ok(size) if size > MAX_FILE_SIZE as u64) {
        if WASM_DIAGNOSTICS {
            crate::klog_fmt!(
                "[WASM-DIAG] read_entire reject path={} reason=too-large\n",
                path
            );
        }
        return Err(FsError::DiskFull);
    }
    let tsc_per_ms = solvent::get_tsc_per_ms();
    let deadline = if tsc_per_ms > 0 {
        (unsafe { core::arch::x86_64::_rdtsc() })
            .wrapping_add(tsc_per_ms.saturating_mul(TIMEOUT_MS))
    } else {
        0
    };
    if WASM_DIAGNOSTICS {
        crate::klog_fmt!("[WASM-DIAG] read_entire open begin path={}\n", path);
    }
    let mut fd = match open_file(path) {
        Ok(fd) => {
            if WASM_DIAGNOSTICS {
                crate::klog_fmt!(
                    "[WASM-DIAG] read_entire open exit path={} fd={}\n",
                    path,
                    fd.fd
                );
            }
            fd
        }
        Err(error) => {
            if WASM_DIAGNOSTICS {
                crate::klog_fmt!(
                    "[WASM-DIAG] read_entire open error path={} error={:?}\n",
                    path,
                    error
                );
            }
            return Err(error);
        }
    };
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut read_calls = 0usize;
    let mut total_bytes = 0usize;
    let result = loop {
        if deadline > 0 && (unsafe { core::arch::x86_64::_rdtsc() }) >= deadline {
            break Err(FsError::Io);
        }
        let call = read_calls + 1;
        if WASM_DIAGNOSTICS && (call <= 4 || call.is_multiple_of(64)) {
            crate::klog_fmt!(
                "[WASM-DIAG] read_entire read begin path={} call={} offset={}\n",
                path,
                call,
                fd.offset
            );
        }
        match read_file(&mut fd, &mut chunk) {
            Ok(0) => {
                if WASM_DIAGNOSTICS {
                    crate::klog_fmt!(
                        "[WASM-DIAG] read_entire eof path={} calls={} bytes={}\n",
                        path,
                        read_calls,
                        total_bytes
                    );
                }
                break Ok(buf);
            }
            Ok(n) if buf.len() + n <= MAX_FILE_SIZE => {
                read_calls += 1;
                total_bytes += n;
                buf.extend_from_slice(&chunk[..n]);
                if WASM_DIAGNOSTICS && (call <= 4 || call.is_multiple_of(64)) {
                    crate::klog_fmt!(
                        "[WASM-DIAG] read_entire read exit path={} call={} bytes={} total={}\n",
                        path,
                        call,
                        n,
                        total_bytes
                    );
                }
            }
            Ok(_) => break Err(FsError::DiskFull),
            Err(e) => break Err(e),
        }
    };
    let _ = close_file(fd);
    match &result {
        Ok(data) if WASM_DIAGNOSTICS => {
            let (len, fingerprint, edges) = diagnostic_fingerprint(data);
            crate::klog_fmt!(
                "[WASM-DIAG] read_entire done path={} len={} fnv=0x{:016x} edges={}\n",
                path,
                len,
                fingerprint,
                edges
            );
        }
        Err(error) if WASM_DIAGNOSTICS => crate::klog_fmt!(
            "[WASM-DIAG] read_entire error path={} calls={} bytes={} error={:?}\n",
            path,
            read_calls,
            total_bytes,
            error
        ),
        _ => {}
    }
    result
}

/// Read a bounded range without materializing the whole file.
pub fn read_file_range(path: &str, offset: u64, limit: usize) -> Result<Vec<u8>, FsError> {
    const MAX_RANGE_BYTES: usize = 64 * 1024;
    const TIMEOUT_MS: u64 = 15_000;
    if limit > MAX_RANGE_BYTES {
        return Err(FsError::InvalidInput);
    }
    let size = file_size(path)?;
    if offset > size {
        return Err(FsError::InvalidSeek);
    }
    let target = (size - offset).min(limit as u64) as usize;
    let deadline = solvent::get_tsc_per_ms()
        .checked_mul(TIMEOUT_MS)
        .map(|ticks| unsafe { core::arch::x86_64::_rdtsc() }.wrapping_add(ticks));
    crate::klog_fmt!(
        "[WASM-DIAG] range begin path={} offset={} limit={} size={} target={}\n",
        path,
        offset,
        limit,
        size,
        target
    );
    let mut fd = open_file(path)?;
    crate::klog_fmt!("[WASM-DIAG] range open exit path={} fd={}\n", path, fd.fd);
    if let Err(error) = seek_file(&mut fd, offset) {
        let _ = close_file(fd);
        return Err(error);
    }
    let mut result = Vec::with_capacity(target);
    let mut chunk = [0u8; 4096];
    while result.len() < target {
        if deadline.is_some_and(|deadline| unsafe { core::arch::x86_64::_rdtsc() } >= deadline) {
            let _ = close_file(fd);
            crate::klog_fmt!(
                "[WASM-DIAG] range timeout path={} offset={} bytes={}\n",
                path,
                offset,
                result.len()
            );
            return Err(FsError::Io);
        }
        let want = (target - result.len()).min(chunk.len());
        crate::klog_fmt!(
            "[WASM-DIAG] range read begin path={} at={} want={} total={}\n",
            path,
            fd.offset,
            want,
            result.len()
        );
        match read_file(&mut fd, &mut chunk[..want]) {
            Ok(0) => {
                crate::klog_fmt!(
                    "[WASM-DIAG] range eof path={} total={}\n",
                    path,
                    result.len()
                );
                break;
            }
            Ok(n) => {
                result.extend_from_slice(&chunk[..n]);
                crate::klog_fmt!(
                    "[WASM-DIAG] range read exit path={} bytes={} total={}\n",
                    path,
                    n,
                    result.len()
                );
            }
            Err(error) => {
                let _ = close_file(fd);
                crate::klog_fmt!(
                    "[WASM-DIAG] range read error path={} error={:?}\n",
                    path,
                    error
                );
                return Err(error);
            }
        }
    }
    close_file(fd)?;
    crate::klog_fmt!(
        "[WASM-DIAG] range end path={} bytes={}\n",
        path,
        result.len()
    );
    Ok(result)
}

/// Temporary diagnostic fingerprint used to compare removable-media copies.
/// It is deliberately cheap enough for the no_std kernel; size plus the edge
/// bytes make short/truncated files immediately apparent as well.
pub(crate) fn diagnostic_fingerprint(data: &[u8]) -> (usize, u64, String) {
    let mut hash = 0xcbf29ce484222325u64;
    for &byte in data {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let mut edges = String::new();
    for &byte in data.iter().take(8) {
        let _ = write!(edges, "{:02x}", byte);
    }
    if data.len() > 8 {
        edges.push('/');
        let tail_start = data.len().saturating_sub(8);
        for &byte in &data[tail_start..] {
            let _ = write!(edges, "{:02x}", byte);
        }
    }
    (data.len(), hash, edges)
}

pub fn read_file_prefix(path: &str, limit: usize) -> Result<Vec<u8>, FsError> {
    const TIMEOUT_MS: u64 = 15_000;
    if limit == 0 {
        return Ok(Vec::new());
    }
    let tsc_per_ms = solvent::get_tsc_per_ms();
    let deadline = if tsc_per_ms > 0 {
        (unsafe { core::arch::x86_64::_rdtsc() })
            .wrapping_add(tsc_per_ms.saturating_mul(TIMEOUT_MS))
    } else {
        0
    };
    let mut fd = open_file(path)?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let result = loop {
        if deadline > 0 && (unsafe { core::arch::x86_64::_rdtsc() }) >= deadline {
            break Err(FsError::Io);
        }
        if buf.len() == limit {
            break Ok(buf);
        }
        let want = (limit - buf.len()).min(chunk.len());
        match read_file(&mut fd, &mut chunk[..want]) {
            Ok(0) => break Ok(buf),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) => break Err(e),
        }
    };
    let _ = close_file(fd);
    result
}

pub fn write_entire_file(path: &str, data: &[u8]) -> Result<(), FsError> {
    if is_dir(path) {
        return Err(FsError::IsADirectory);
    }
    if exists(path) {
        let _ = remove(path);
    }
    create_file(path, data)
}

pub fn file_size(path: &str) -> Result<u64, FsError> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return Ok(0);
    }
    let (parent, name) = trimmed.rsplit_once('/').unwrap_or((".", trimmed));
    let parent = if parent.is_empty() { "/" } else { parent };
    let entries = list_dir(parent)?;
    entries
        .iter()
        .find(|e| e.name == name)
        .map(|e| e.size)
        .ok_or(FsError::FileNotFound)
}

// ── Package management ─────────────────────────────────────

pub fn list_packages() -> Result<Vec<PackageEntry>, FsError> {
    let mut packages = Vec::new();
    if !exists("/packages") {
        create_dir("/packages")?;
        return Ok(packages);
    }
    let entries = list_dir("/packages")?;
    for entry in &entries {
        let manifest_path = alloc::format!("/packages/{}/manifest.txt", entry.name);
        if entry.is_dir
            && exists(&manifest_path)
            && let Ok(data) = read_entire_file(&manifest_path)
            && let Ok(text) = core::str::from_utf8(&data)
            && let Some(pkg) = parse_manifest(&entry.name, text)
        {
            packages.push(pkg);
        }
    }
    Ok(packages)
}

pub fn install_package(
    name: &str,
    version: &str,
    description: &str,
    binary: &[u8],
) -> Result<(), FsError> {
    install_package_with_runtime(name, version, description, "native", binary)
}

pub fn install_package_with_runtime(
    name: &str,
    version: &str,
    description: &str,
    runtime: &str,
    binary: &[u8],
) -> Result<(), FsError> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || !matches!(runtime, "native" | "linux")
    {
        return Err(FsError::InvalidInput);
    }
    let pkg_dir = alloc::format!("/packages/{}", name);
    if exists(&pkg_dir) {
        return Err(FsError::FileExists);
    }
    create_dir(&pkg_dir)?;

    let manifest = alloc::format!(
        "name = \"{}\"\nversion = \"{}\"\ndescription = \"{}\"\nbinary = \"app.bin\"\nruntime = \"{}\"\n",
        name,
        version,
        description,
        runtime
    );
    let manifest_path = alloc::format!("/packages/{}/manifest.txt", name);
    write_entire_file(&manifest_path, manifest.as_bytes())?;

    let bin_path = alloc::format!("/packages/{}/app.bin", name);
    write_entire_file(&bin_path, binary)?;

    Ok(())
}

pub fn remove_package(name: &str) -> Result<(), FsError> {
    let pkg_dir = alloc::format!("/packages/{}", name);
    if !exists(&pkg_dir) {
        return Err(FsError::FileNotFound);
    }
    let mut sorted = walk_dir(&pkg_dir)?;
    sorted.sort_by(|a, b| b.len().cmp(&a.len()));
    sorted.iter().try_for_each(|e| remove(e))?;
    remove(&pkg_dir)
}
