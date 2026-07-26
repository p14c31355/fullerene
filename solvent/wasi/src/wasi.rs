use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::str;
use wasmi::{AsContext, Caller, Error, Memory};

// ── WASI errno ─────────────────────────────────────────────────────

pub const ESUCCESS: u32 = 0;
pub const EACCES: u32 = 2;
pub const EBADF: u32 = 8;
pub const EEXIST: u32 = 20;
pub const EINVAL: u32 = 28;
pub const EIO: u32 = 29;
pub const EISDIR: u32 = 31;
pub const ENOENT: u32 = 44;
pub const ENOSPC: u32 = 52;
pub const ENOTDIR: u32 = 54;
pub const ENOTEMPTY: u32 = 55;
pub const ENOTSUP: u32 = 58;

// ── WASI file types ───────────────────────────────────────────────

pub const FILETYPE_DIRECTORY: u8 = 3;
pub const FILETYPE_REGULAR_FILE: u8 = 4;
pub const FILETYPE_CHARACTER_DEVICE: u8 = 2;

// ── WASI fdflags ───────────────────────────────────────────────

pub const FDFLAG_APPEND: u16 = 1;
pub const FDFLAG_DSYNC: u16 = 2;
pub const FDFLAG_NONBLOCK: u16 = 4;
pub const FDFLAG_RSYNC: u16 = 8;
pub const FDFLAG_SYNC: u16 = 16;

// ── WASI rights ───────────────────────────────────────────────────

pub const RIGHT_FD_READ: u64 = 1 << 1;
pub const RIGHT_FD_WRITE: u64 = 1 << 6;
pub const RIGHT_FD_SEEK: u64 = 1 << 2;
pub const RIGHT_FD_TELL: u64 = 1 << 5;
pub const RIGHT_FD_FILESTAT_GET: u64 = 1 << 21;
pub const RIGHT_PATH_OPEN: u64 = 1 << 13;
pub const RIGHT_FD_READDIR: u64 = 1 << 14;
pub const RIGHT_PATH_FILESTAT_GET: u64 = 1 << 18;

// ── WASI whence ───────────────────────────────────────────────────

pub const WHENCE_SET: u32 = 0;
pub const WHENCE_CUR: u32 = 1;
pub const WHENCE_END: u32 = 2;

// ── WASI clock ids ────────────────────────────────────────────────

pub const CLOCK_REALTIME: u32 = 0;
pub const CLOCK_MONOTONIC: u32 = 1;

// ── FD table entry ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum WasiFd {
    Stdin,
    Stdout,
    Stderr,
    PreopenedDir {
        path: String,
    },
    File {
        path: String,
        size: u64,
        offset: u64,
        cache_offset: u64,
        cache: Vec<u8>,
        writable: bool,
        dirty: bool,
        write_data: Vec<u8>,
    },
}

// ── WASI context ─────────────────────────────────────────────────

const MAX_OPEN_FDS: usize = 128;
const MAX_WRITE_FILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_DISPLAY_RGB_BYTES: u32 = 800 * 600 * 3;
const MAX_CAPTURE_RGBA_BYTES: u32 = 32 * 1024 * 1024;
const MAX_CAPTURE_CHUNK_BYTES: u32 = 256 * 1024;
const MAX_WINDOW_TITLE_BYTES: u32 = 1024;
const MAX_TEXT_BYTES: u32 = 512 * 1024;
const MAX_ERROR_BYTES: u32 = 64 * 1024;

pub struct WasiCtx {
    pub exit_code: Option<u32>,
    pub args: Vec<Vec<u8>>,
    pub env: Vec<Vec<u8>>,
    pub fds: BTreeMap<u32, WasiFd>,
    pub next_fd: u32,
    pub write_stdout: fn(&[u8]),
    pub write_stderr: fn(&[u8]),
    pub read_stdin: fn() -> Option<u8>,
    pub yield_now: fn(),
    pub file_size: fn(&str) -> Result<u64, genome::FsError>,
    pub read_file_range: fn(&str, u64, usize) -> Result<Vec<u8>, genome::FsError>,
    pub read_directory: fn(&str) -> Result<Vec<(String, u8)>, genome::FsError>,
    pub write_file: fn(&str, &[u8]) -> Result<(), genome::FsError>,
    pub get_monotonic_ns: fn() -> u64,
    pub screen_dimensions: fn() -> (u32, u32),
    pub capture_screen: fn() -> Option<(u32, u32, Vec<u8>)>,
    pub capture_screen_chunk: fn(u32, &mut [u8]) -> Option<(u32, u32)>,
    pub show_image: fn(u32, u32, &[u8]) -> i32,
    pub show_text: fn(&str, &str) -> i32,
    pub show_error: fn(&str, &str) -> i32,
    pub create_window: fn(&str, u32, u32) -> i32,
    pub update_window: fn(i32, u32, u32, &[u8]) -> i32,
    pub close_window: fn(i32) -> i32,
}

impl WasiCtx {
    pub fn new(
        args: &[&str],
        write_stdout: fn(&[u8]),
        write_stderr: fn(&[u8]),
        read_stdin: fn() -> Option<u8>,
        yield_now: fn(),
        file_size: fn(&str) -> Result<u64, genome::FsError>,
        read_file_range: fn(&str, u64, usize) -> Result<Vec<u8>, genome::FsError>,
        read_directory: fn(&str) -> Result<Vec<(String, u8)>, genome::FsError>,
        write_file: fn(&str, &[u8]) -> Result<(), genome::FsError>,
        get_monotonic_ns: fn() -> u64,
        screen_dimensions: fn() -> (u32, u32),
        capture_screen: fn() -> Option<(u32, u32, Vec<u8>)>,
        capture_screen_chunk: fn(u32, &mut [u8]) -> Option<(u32, u32)>,
        show_image: fn(u32, u32, &[u8]) -> i32,
        show_text: fn(&str, &str) -> i32,
        show_error: fn(&str, &str) -> i32,
        create_window: fn(&str, u32, u32) -> i32,
        update_window: fn(i32, u32, u32, &[u8]) -> i32,
        close_window: fn(i32) -> i32,
    ) -> Self {
        let args_vec: Vec<Vec<u8>> = args
            .iter()
            .map(|s| {
                let mut v = Vec::from(s.as_bytes());
                v.push(0);
                v
            })
            .collect();
        let mut fds = BTreeMap::new();
        fds.insert(0, WasiFd::Stdin);
        fds.insert(1, WasiFd::Stdout);
        fds.insert(2, WasiFd::Stderr);
        fds.insert(
            3,
            WasiFd::PreopenedDir {
                path: String::from("/"),
            },
        );
        Self {
            exit_code: None,
            args: args_vec,
            env: Vec::new(),
            fds,
            next_fd: 4,
            write_stdout,
            write_stderr,
            read_stdin,
            yield_now,
            file_size,
            read_file_range,
            read_directory,
            write_file,
            get_monotonic_ns,
            screen_dimensions,
            capture_screen,
            capture_screen_chunk,
            show_image,
            show_text,
            show_error,
            create_window,
            update_window,
            close_window,
        }
    }
}

// ── Memory helpers ────────────────────────────────────────────────

fn get_memory(caller: &Caller<'_, WasiCtx>) -> Result<Memory, Error> {
    caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| Error::new("wasm module missing memory export"))
}

fn read_u32(memory: &Memory, ctx: impl AsContext, addr: u32) -> Result<u32, Error> {
    let mut buf = [0u8; 4];
    memory
        .read(ctx, addr as usize, &mut buf)
        .map_err(|_| Error::new("memory read failed"))?;
    Ok(u32::from_le_bytes(buf))
}

fn write_u32(
    memory: &Memory,
    ctx: impl wasmi::AsContextMut,
    addr: u32,
    val: u32,
) -> Result<(), Error> {
    let buf = val.to_le_bytes();
    memory
        .write(ctx, addr as usize, &buf)
        .map_err(|_| Error::new("memory write failed"))
}

fn write_u64(
    memory: &Memory,
    ctx: impl wasmi::AsContextMut,
    addr: u32,
    val: u64,
) -> Result<(), Error> {
    let buf = val.to_le_bytes();
    memory
        .write(ctx, addr as usize, &buf)
        .map_err(|_| Error::new("memory write failed"))
}

fn write_u8(
    memory: &Memory,
    ctx: impl wasmi::AsContextMut,
    addr: u32,
    val: u8,
) -> Result<(), Error> {
    let buf = [val];
    memory
        .write(ctx, addr as usize, &buf)
        .map_err(|_| Error::new("memory write failed"))
}

fn map_fs_error(err: &genome::FsError) -> u32 {
    use genome::FsError;
    match err {
        FsError::FileNotFound => ENOENT,
        FsError::FileExists => EEXIST,
        FsError::PermissionDenied => EACCES,
        FsError::InvalidFileDescriptor => EBADF,
        FsError::InvalidSeek => EINVAL,
        FsError::DiskFull => ENOSPC,
        FsError::NotADirectory => ENOTDIR,
        FsError::DirectoryNotEmpty => ENOTEMPTY,
        FsError::IsADirectory => EISDIR,
        FsError::InvalidPath => EINVAL,
        FsError::NotSupported => ENOTSUP,
        FsError::InvalidInput => EINVAL,
        FsError::UnexpectedEof => EIO,
        FsError::Io => EIO,
    }
}

// ── Host function implementations ─────────────────────────────────

pub fn fd_write(
    mut caller: Caller<'_, WasiCtx>,
    fd: u32,
    iovs_ptr: u32,
    iovs_len: u32,
    nwritten_ptr: u32,
) -> Result<u32, Error> {
    let memory = get_memory(&caller)?;
    let mut total: u32 = 0;
    for i in 0..iovs_len {
        let base =
            match iovs_ptr.checked_add(i.checked_mul(8).ok_or_else(|| Error::new("overflow"))?) {
                Some(b) => b,
                None => return Ok(EINVAL),
            };
        let buf_ptr = read_u32(&memory, &caller, base)?;
        let len_addr = match base.checked_add(4) {
            Some(a) => a,
            None => return Ok(EINVAL),
        };
        let buf_len = read_u32(&memory, &caller, len_addr)?;
        let mut offset = 0;
        let mut temp_buf = [0u8; 4096];
        while offset < buf_len {
            let chunk_len = (buf_len - offset).min(4096) as usize;
            memory
                .read(
                    &caller,
                    buf_ptr as usize + offset as usize,
                    &mut temp_buf[..chunk_len],
                )
                .map_err(|_| Error::new("fd_write: read iov failed"))?;
            match fd {
                1 => (caller.data().write_stdout)(&temp_buf[..chunk_len]),
                2 => (caller.data().write_stderr)(&temp_buf[..chunk_len]),
                _ => {
                    let ctx = caller.data_mut();
                    let Some(WasiFd::File {
                        writable: true,
                        offset,
                        size,
                        dirty,
                        write_data,
                        ..
                    }) = ctx.fds.get_mut(&fd)
                    else {
                        return Ok(EBADF);
                    };
                    let start = usize::try_from(*offset)
                        .map_err(|_| Error::new("fd_write: offset overflow"))?;
                    let end = start
                        .checked_add(chunk_len)
                        .ok_or_else(|| Error::new("fd_write: size overflow"))?;
                    if end > MAX_WRITE_FILE_BYTES {
                        return Ok(ENOSPC);
                    }
                    if write_data.len() < end {
                        write_data.resize(end, 0);
                    }
                    write_data[start..end].copy_from_slice(&temp_buf[..chunk_len]);
                    *offset = end as u64;
                    *size = (*size).max(*offset);
                    *dirty = true;
                }
            }
            offset += chunk_len as u32;
            total = total.saturating_add(chunk_len as u32);
        }
    }
    write_u32(&memory, &mut caller, nwritten_ptr, total)?;
    Ok(ESUCCESS)
}

pub fn fd_read(
    mut caller: Caller<'_, WasiCtx>,
    fd: u32,
    iovs_ptr: u32,
    iovs_len: u32,
    nread_ptr: u32,
) -> Result<u32, Error> {
    if iovs_len == 0 {
        let memory = get_memory(&caller)?;
        write_u32(&memory, &mut caller, nread_ptr, 0)?;
        return Ok(ESUCCESS);
    }
    let memory = get_memory(&caller)?;
    let mut total_read: u32 = 0;
    match fd {
        0 => {
            let mut temp_buf = [0u8; 4096];
            let mut first_byte_opt: Option<u8> = None;
            for i in 0..iovs_len {
                let base = match iovs_ptr
                    .checked_add(i.checked_mul(8).ok_or_else(|| Error::new("overflow"))?)
                {
                    Some(b) => b,
                    None => return Ok(EINVAL),
                };
                let buf_ptr = read_u32(&memory, &caller, base)?;
                let len_addr = match base.checked_add(4) {
                    Some(a) => a,
                    None => return Ok(EINVAL),
                };
                let buf_len = read_u32(&memory, &caller, len_addr)?;
                if buf_len == 0 {
                    continue;
                }
                // Non-blocking: if no byte is available, return 0 immediately.
                // WASM runs synchronously inside the kernel shell, so blocking
                // here would freeze the entire desktop with no recovery path.
                if first_byte_opt.is_none() {
                    match (caller.data().read_stdin)() {
                        Some(byte) => first_byte_opt = Some(byte),
                        None => {
                            write_u32(&memory, &mut caller, nread_ptr, 0)?;
                            return Ok(ESUCCESS);
                        }
                    }
                }
                let mut iov_written: u32 = 0;
                while iov_written < buf_len {
                    let chunk_len = (buf_len - iov_written).min(4096) as usize;
                    let mut chunk_read = 0;
                    // Use the first byte from the wait loop if this is the first chunk.
                    if total_read == 0 && chunk_read == 0 {
                        if let Some(first_byte) = first_byte_opt {
                            temp_buf[0] = first_byte;
                            chunk_read = 1;
                        }
                    }
                    for j in chunk_read..chunk_len {
                        match (caller.data().read_stdin)() {
                            Some(byte) => {
                                temp_buf[j] = byte;
                                chunk_read += 1;
                            }
                            None => break,
                        }
                    }
                    if chunk_read == 0 {
                        break;
                    }
                    memory
                        .write(
                            &mut caller,
                            buf_ptr as usize + iov_written as usize,
                            &temp_buf[..chunk_read],
                        )
                        .map_err(|_| Error::new("fd_read: write failed"))?;
                    iov_written += chunk_read as u32;
                    total_read += chunk_read as u32;
                    if chunk_read < chunk_len {
                        break;
                    }
                }
                if iov_written < buf_len {
                    break;
                }
            }
            write_u32(&memory, &mut caller, nread_ptr, total_read)?;
            Ok(ESUCCESS)
        }
        _ => {
            for i in 0..iovs_len {
                let base = match iovs_ptr
                    .checked_add(i.checked_mul(8).ok_or_else(|| Error::new("overflow"))?)
                {
                    Some(b) => b,
                    None => return Ok(EINVAL),
                };
                let buf_ptr = read_u32(&memory, &caller, base)?;
                let len_addr = match base.checked_add(4) {
                    Some(a) => a,
                    None => return Ok(EINVAL),
                };
                let buf_len = read_u32(&memory, &caller, len_addr)?;
                let mut iov_written = 0usize;
                while iov_written < buf_len as usize {
                    let (path, size, offset, cache_offset, cache_len) = {
                        let bc = caller.data();
                        match bc.fds.get(&fd) {
                            Some(WasiFd::File {
                                path,
                                size,
                                offset,
                                cache_offset,
                                cache,
                                ..
                            }) => (path.clone(), *size, *offset, *cache_offset, cache.len()),
                            _ => return Ok(EBADF),
                        }
                    };
                    if offset >= size {
                        break;
                    }

                    let in_cache = offset >= cache_offset
                        && offset < cache_offset.saturating_add(cache_len as u64);
                    if !in_cache {
                        const FILE_CACHE_BYTES: usize = 64 * 1024;
                        let fetch_len = (size - offset).min(FILE_CACHE_BYTES as u64) as usize;
                        let read_file_range = caller.data().read_file_range;
                        let fetched = match read_file_range(&path, offset, fetch_len) {
                            Ok(bytes) => bytes,
                            Err(error) => return Ok(map_fs_error(&error)),
                        };
                        if fetched.is_empty() {
                            break;
                        }
                        let bc = caller.data_mut();
                        let Some(WasiFd::File {
                            cache_offset,
                            cache,
                            ..
                        }) = bc.fds.get_mut(&fd)
                        else {
                            return Ok(EBADF);
                        };
                        *cache_offset = offset;
                        *cache = fetched;
                        continue;
                    }

                    let copy_len = (buf_len as usize - iov_written).min(
                        cache_offset
                            .saturating_add(cache_len as u64)
                            .saturating_sub(offset) as usize,
                    );
                    let chunk = {
                        let bc = caller.data();
                        match bc.fds.get(&fd) {
                            Some(WasiFd::File {
                                cache_offset,
                                cache,
                                ..
                            }) => {
                                let start = (offset - *cache_offset) as usize;
                                cache[start..start + copy_len].to_vec()
                            }
                            _ => return Ok(EBADF),
                        }
                    };
                    memory
                        .write(&mut caller, buf_ptr as usize + iov_written, &chunk)
                        .map_err(|_| Error::new("fd_read: write data failed"))?;
                    let bc = caller.data_mut();
                    if let Some(WasiFd::File { offset: o, .. }) = bc.fds.get_mut(&fd) {
                        *o = o.saturating_add(copy_len as u64);
                    }
                    iov_written += copy_len;
                    total_read += copy_len as u32;
                }
                if iov_written < buf_len as usize {
                    break;
                }
            }
            write_u32(&memory, &mut caller, nread_ptr, total_read)?;
            Ok(ESUCCESS)
        }
    }
}

pub fn fd_fdstat_get(mut caller: Caller<'_, WasiCtx>, fd: u32, buf_ptr: u32) -> Result<u32, Error> {
    let (filetype, flags, rights_base, rights_inheriting) = {
        let bc = caller.data();
        match bc.fds.get(&fd) {
            Some(WasiFd::Stdin) => (FILETYPE_CHARACTER_DEVICE, 0u16, RIGHT_FD_READ, 0u64),
            Some(WasiFd::Stdout) => (FILETYPE_CHARACTER_DEVICE, 0u16, RIGHT_FD_WRITE, 0u64),
            Some(WasiFd::Stderr) => (FILETYPE_CHARACTER_DEVICE, 0u16, RIGHT_FD_WRITE, 0u64),
            Some(WasiFd::PreopenedDir { .. }) => (
                FILETYPE_DIRECTORY,
                0u16,
                RIGHT_FD_READDIR
                    | RIGHT_PATH_OPEN
                    | RIGHT_PATH_FILESTAT_GET
                    | RIGHT_FD_FILESTAT_GET,
                RIGHT_FD_READ
                    | RIGHT_FD_WRITE
                    | RIGHT_FD_SEEK
                    | RIGHT_FD_TELL
                    | RIGHT_FD_FILESTAT_GET,
            ),
            Some(WasiFd::File { .. }) => (
                FILETYPE_REGULAR_FILE,
                0u16,
                RIGHT_FD_READ
                    | RIGHT_FD_WRITE
                    | RIGHT_FD_SEEK
                    | RIGHT_FD_TELL
                    | RIGHT_FD_FILESTAT_GET,
                0u64,
            ),
            None => return Ok(EBADF),
        }
    };
    let memory = get_memory(&caller)?;
    // fdstat layout: fs_filetype(u8) + padding + fs_flags(u16) + padding + fs_rights_base(u64) + fs_rights_inheriting(u64)
    let off_flags = buf_ptr
        .checked_add(2)
        .ok_or_else(|| Error::new("overflow"))?;
    let off_rights = buf_ptr
        .checked_add(8)
        .ok_or_else(|| Error::new("overflow"))?;
    let off_inheriting = buf_ptr
        .checked_add(16)
        .ok_or_else(|| Error::new("overflow"))?;
    write_u8(&memory, &mut caller, buf_ptr, filetype)?;
    write_u32(&memory, &mut caller, off_flags, flags as u32)?;
    write_u64(&memory, &mut caller, off_rights, rights_base)?;
    write_u64(&memory, &mut caller, off_inheriting, rights_inheriting)?;
    Ok(ESUCCESS)
}

pub fn fd_close(mut caller: Caller<'_, WasiCtx>, fd: u32) -> Result<u32, Error> {
    if fd <= 3 {
        return Ok(ENOTSUP);
    }
    let pending_write = {
        let ctx = caller.data();
        match ctx.fds.get(&fd) {
            Some(WasiFd::File {
                path,
                writable: true,
                dirty: true,
                write_data,
                ..
            }) => Some((path.clone(), write_data.clone())),
            Some(WasiFd::File { .. }) => None,
            Some(_) => return Ok(EBADF),
            None => return Ok(EBADF),
        }
    };
    if let Some((path, data)) = pending_write {
        let write_file = caller.data().write_file;
        if let Err(error) = write_file(&path, &data) {
            return Ok(map_fs_error(&error));
        }
    }
    let ctx = caller.data_mut();
    if ctx.fds.remove(&fd).is_some() {
        Ok(ESUCCESS)
    } else {
        Ok(EBADF)
    }
}

pub fn fd_seek(
    mut caller: Caller<'_, WasiCtx>,
    fd: u32,
    offset: i64,
    whence: u32,
    newoffset_ptr: u32,
) -> Result<u32, Error> {
    let file_len = {
        let bc = caller.data();
        match bc.fds.get(&fd) {
            Some(WasiFd::File { size, .. }) => *size,
            _ => return Ok(EBADF),
        }
    };
    let current_offset = {
        let bc = caller.data();
        match bc.fds.get(&fd) {
            Some(WasiFd::File { offset, .. }) => *offset,
            _ => 0,
        }
    };
    let new_offset = match whence {
        WHENCE_SET => Some(offset),
        WHENCE_CUR => (current_offset as i64).checked_add(offset),
        WHENCE_END => (file_len as i64).checked_add(offset),
        _ => return Ok(EINVAL),
    };
    let Some(new_offset) = new_offset else {
        return Ok(EINVAL);
    };
    if new_offset < 0 {
        return Ok(EINVAL);
    }
    let new_offset = new_offset as u64;
    {
        let bc = caller.data_mut();
        if let Some(WasiFd::File { offset: o, .. }) = bc.fds.get_mut(&fd) {
            *o = new_offset;
        }
    }
    let memory = get_memory(&caller)?;
    write_u64(&memory, &mut caller, newoffset_ptr, new_offset as u64)?;
    Ok(ESUCCESS)
}

pub fn fd_prestat_get(mut caller: Caller<'_, WasiCtx>, fd: u32, buf: u32) -> Result<u32, Error> {
    if fd != 3 {
        return Ok(EBADF);
    }
    let memory = get_memory(&caller)?;
    let name_len = {
        let bc = caller.data();
        match bc.fds.get(&fd) {
            Some(WasiFd::PreopenedDir { path }) => path.len() as u32,
            _ => return Ok(EBADF),
        }
    };
    let off = buf.checked_add(4).ok_or_else(|| Error::new("overflow"))?;
    write_u8(&memory, &mut caller, buf, 0)?;
    write_u32(&memory, &mut caller, off, name_len)?;
    Ok(ESUCCESS)
}

pub fn fd_prestat_dir_name(
    mut caller: Caller<'_, WasiCtx>,
    fd: u32,
    path_ptr: u32,
    path_len: u32,
) -> Result<u32, Error> {
    if fd != 3 {
        return Ok(EBADF);
    }
    let path = {
        let bc = caller.data();
        match bc.fds.get(&fd) {
            Some(WasiFd::PreopenedDir { path }) => path.clone(),
            _ => return Ok(EBADF),
        }
    };
    let memory = get_memory(&caller)?;
    let len = (path.len() as u32).min(path_len);
    memory
        .write(
            &mut caller,
            path_ptr as usize,
            &path.as_bytes()[..len as usize],
        )
        .map_err(|_| Error::new("fd_prestat_dir_name: write failed"))?;
    Ok(ESUCCESS)
}

pub fn path_open(
    mut caller: Caller<'_, WasiCtx>,
    fd: u32,
    _dirflags: u32,
    path_ptr: u32,
    path_len: u32,
    oflags: u32,
    fs_rights_base: u64,
    _fs_rights_inheriting: u64,
    _fdflags: u32,
    result_fd_ptr: u32,
) -> Result<u32, Error> {
    if fd != 3 {
        return Ok(EBADF);
    }
    if path_len > 1024 {
        return Ok(EINVAL);
    }
    let memory = get_memory(&caller)?;
    let mut path_buf = vec![0u8; path_len as usize];
    memory
        .read(&caller, path_ptr as usize, &mut path_buf)
        .map_err(|_| Error::new("path_open: read path failed"))?;
    let path_str = match str::from_utf8(&path_buf) {
        Ok(s) => s,
        Err(_) => return Ok(EINVAL),
    };
    let clean = path_str.trim_matches('\0').trim_start_matches('/');
    let full_path = if clean.is_empty() {
        String::from("/")
    } else {
        alloc::format!("/{}", clean)
    };
    const OFLAGS_CREAT: u32 = 1;
    const OFLAGS_EXCL: u32 = 4;
    const OFLAGS_TRUNC: u32 = 8;
    let writable = fs_rights_base & RIGHT_FD_WRITE != 0;
    let truncate = oflags & OFLAGS_TRUNC != 0;
    if truncate && !writable {
        return Ok(EACCES);
    }
    let existing_size = {
        let bc = caller.data();
        (bc.file_size)(&full_path)
    };
    let (size, exists) = match existing_size {
        Ok(size) => {
            if oflags & OFLAGS_EXCL != 0 && oflags & OFLAGS_CREAT != 0 {
                return Ok(EEXIST);
            }
            if truncate { (0, true) } else { (size, true) }
        }
        Err(genome::FsError::FileNotFound) if oflags & OFLAGS_CREAT != 0 => (0, false),
        Err(error) => return Ok(map_fs_error(&error)),
    };
    if exists && size > MAX_WRITE_FILE_BYTES as u64 && writable {
        return Ok(ENOSPC);
    }

    let mut write_data = Vec::new();
    if writable && exists && !truncate {
        let read_file_range = caller.data().read_file_range;
        let mut offset = 0u64;
        while offset < size {
            let want = (size - offset).min(64 * 1024) as usize;
            let chunk = match read_file_range(&full_path, offset, want) {
                Ok(chunk) => chunk,
                Err(error) => return Ok(map_fs_error(&error)),
            };
            if chunk.is_empty() {
                return Ok(EIO);
            }
            offset = offset.saturating_add(chunk.len() as u64);
            write_data.extend_from_slice(&chunk);
        }
    }

    let fd_count = caller.data().fds.len();
    if fd_count >= MAX_OPEN_FDS {
        return Ok(ENOSPC);
    }
    let new_fd = {
        let bc = caller.data_mut();
        let fd = bc.next_fd;
        bc.next_fd += 1;
        bc.fds.insert(
            fd,
            WasiFd::File {
                path: full_path,
                size,
                offset: 0,
                cache_offset: 0,
                cache: Vec::new(),
                writable,
                // A create/truncate open must be committed even when the
                // guest writes zero bytes (for example fs::write(path, [])).
                dirty: writable && (truncate || !exists),
                write_data,
            },
        );
        fd
    };
    write_u32(&memory, &mut caller, result_fd_ptr, new_fd)?;
    Ok(ESUCCESS)
}

pub fn path_filestat_get(
    mut caller: Caller<'_, WasiCtx>,
    _fd: u32,
    _flags: u32,
    path_ptr: u32,
    path_len: u32,
    buf_ptr: u32,
) -> Result<u32, Error> {
    if path_len > 1024 {
        return Ok(EINVAL);
    }
    let memory = get_memory(&caller)?;
    let mut path_buf = vec![0u8; path_len as usize];
    memory
        .read(&caller, path_ptr as usize, &mut path_buf)
        .map_err(|_| Error::new("path_filestat_get: read path failed"))?;
    let path_str = match str::from_utf8(&path_buf) {
        Ok(s) => s,
        Err(_) => return Ok(EINVAL),
    };
    let clean = path_str.trim_matches('\0').trim_start_matches('/');
    let full_path = if clean.is_empty() {
        String::from("/")
    } else {
        alloc::format!("/{}", clean)
    };
    let size = {
        let bc = caller.data();
        match (bc.file_size)(&full_path) {
            Ok(size) => size,
            Err(_) => return Ok(ENOENT),
        }
    };
    let off_dev = buf_ptr
        .checked_add(8)
        .ok_or_else(|| Error::new("overflow"))?;
    let off_type = buf_ptr
        .checked_add(16)
        .ok_or_else(|| Error::new("overflow"))?;
    let off_nlink = buf_ptr
        .checked_add(24)
        .ok_or_else(|| Error::new("overflow"))?;
    let off_size = buf_ptr
        .checked_add(32)
        .ok_or_else(|| Error::new("overflow"))?;
    let off_atim = buf_ptr
        .checked_add(40)
        .ok_or_else(|| Error::new("overflow"))?;
    let off_mtim = buf_ptr
        .checked_add(48)
        .ok_or_else(|| Error::new("overflow"))?;
    let off_ctim = buf_ptr
        .checked_add(56)
        .ok_or_else(|| Error::new("overflow"))?;
    write_u64(&memory, &mut caller, buf_ptr, 0)?;
    write_u64(&memory, &mut caller, off_dev, 1)?;
    write_u8(&memory, &mut caller, off_type, FILETYPE_REGULAR_FILE)?;
    write_u64(&memory, &mut caller, off_nlink, 1)?;
    write_u64(&memory, &mut caller, off_size, size)?;
    write_u64(&memory, &mut caller, off_atim, 0)?;
    write_u64(&memory, &mut caller, off_mtim, 0)?;
    write_u64(&memory, &mut caller, off_ctim, 0)?;
    Ok(ESUCCESS)
}

pub fn fd_filestat_get(
    mut caller: Caller<'_, WasiCtx>,
    fd: u32,
    buf_ptr: u32,
) -> Result<u32, Error> {
    let (filetype, size) = {
        let bc = caller.data();
        match bc.fds.get(&fd) {
            Some(WasiFd::File { size, .. }) => (FILETYPE_REGULAR_FILE, *size),
            Some(WasiFd::PreopenedDir { .. }) => (FILETYPE_DIRECTORY, 0u64),
            _ => return Ok(EBADF),
        }
    };
    let memory = get_memory(&caller)?;
    let off_dev = buf_ptr
        .checked_add(8)
        .ok_or_else(|| Error::new("overflow"))?;
    let off_type = buf_ptr
        .checked_add(16)
        .ok_or_else(|| Error::new("overflow"))?;
    let off_nlink = buf_ptr
        .checked_add(24)
        .ok_or_else(|| Error::new("overflow"))?;
    let off_size = buf_ptr
        .checked_add(32)
        .ok_or_else(|| Error::new("overflow"))?;
    let off_atim = buf_ptr
        .checked_add(40)
        .ok_or_else(|| Error::new("overflow"))?;
    let off_mtim = buf_ptr
        .checked_add(48)
        .ok_or_else(|| Error::new("overflow"))?;
    let off_ctim = buf_ptr
        .checked_add(56)
        .ok_or_else(|| Error::new("overflow"))?;
    write_u64(&memory, &mut caller, buf_ptr, 0)?;
    write_u64(&memory, &mut caller, off_dev, fd as u64)?;
    write_u8(&memory, &mut caller, off_type, filetype)?;
    write_u64(&memory, &mut caller, off_nlink, 1)?;
    write_u64(&memory, &mut caller, off_size, size)?;
    write_u64(&memory, &mut caller, off_atim, 0)?;
    write_u64(&memory, &mut caller, off_mtim, 0)?;
    write_u64(&memory, &mut caller, off_ctim, 0)?;
    Ok(ESUCCESS)
}

pub fn fd_readdir(
    mut caller: Caller<'_, WasiCtx>,
    fd: u32,
    buf_ptr: u32,
    buf_len: u32,
    cookie: u64,
    bufused_ptr: u32,
) -> Result<u32, Error> {
    let entries = {
        let bc = caller.data();
        match bc.fds.get(&fd) {
            Some(WasiFd::PreopenedDir { path }) => match (bc.read_directory)(path) {
                Ok(entries) => entries,
                Err(e) => return Ok(map_fs_error(&e)),
            },
            _ => return Ok(EBADF),
        }
    };
    let memory = get_memory(&caller)?;
    let mut used: u32 = 0;
    let cookie_start = cookie as usize;
    let start_entry = if cookie_start == 0 {
        let name = b".";
        let entry_size = 24 + name.len() as u32;
        if entry_size <= buf_len {
            let off_next = buf_ptr
                .checked_add(8)
                .ok_or_else(|| Error::new("overflow"))?;
            let off_namelen = buf_ptr
                .checked_add(16)
                .ok_or_else(|| Error::new("overflow"))?;
            let off_type = buf_ptr
                .checked_add(20)
                .ok_or_else(|| Error::new("overflow"))?;
            let off_name = buf_ptr
                .checked_add(24)
                .ok_or_else(|| Error::new("overflow"))?;
            write_u64(&memory, &mut caller, buf_ptr, 1)?;
            write_u64(&memory, &mut caller, off_next, 0)?;
            write_u32(&memory, &mut caller, off_namelen, name.len() as u32)?;
            write_u8(&memory, &mut caller, off_type, FILETYPE_DIRECTORY)?;
            memory
                .write(&mut caller, off_name as usize, name)
                .map_err(|_| Error::new("fd_readdir: write name failed"))?;
            used += entry_size;
        }
        0usize
    } else {
        cookie_start.saturating_sub(1)
    };
    for entry_idx in start_entry..entries.len() {
        let (ref name, filetype) = entries[entry_idx];
        let name_bytes = name.as_bytes();
        let entry_size = 24 + name_bytes.len() as u32;
        if used + entry_size > buf_len {
            break;
        }
        let off = buf_ptr
            .checked_add(used)
            .ok_or_else(|| Error::new("overflow"))?;
        let off_next = off.checked_add(8).ok_or_else(|| Error::new("overflow"))?;
        let off_namelen = off.checked_add(16).ok_or_else(|| Error::new("overflow"))?;
        let off_type = off.checked_add(20).ok_or_else(|| Error::new("overflow"))?;
        let off_name = off.checked_add(24).ok_or_else(|| Error::new("overflow"))?;
        let next_cookie = entry_idx + 2;
        write_u64(&memory, &mut caller, off, next_cookie as u64)?;
        write_u64(&memory, &mut caller, off_next, entry_idx as u64)?;
        write_u32(&memory, &mut caller, off_namelen, name_bytes.len() as u32)?;
        write_u8(&memory, &mut caller, off_type, filetype)?;
        memory
            .write(&mut caller, off_name as usize, name_bytes)
            .map_err(|_| Error::new("fd_readdir: write name failed"))?;
        used += entry_size;
    }
    write_u32(&memory, &mut caller, bufused_ptr, used)?;
    Ok(ESUCCESS)
}

pub fn proc_exit(mut caller: Caller<'_, WasiCtx>, code: u32) -> Result<(), Error> {
    caller.data_mut().exit_code = Some(code);
    Err(Error::new("proc_exit"))
}

pub fn environ_sizes_get(
    mut caller: Caller<'_, WasiCtx>,
    count_ptr: u32,
    buf_size_ptr: u32,
) -> Result<u32, Error> {
    let memory = get_memory(&caller)?;
    let (count, buf_size) = {
        let bc = caller.data();
        (
            bc.env.len() as u32,
            bc.env.iter().map(|e| e.len() as u32).sum::<u32>(),
        )
    };
    write_u32(&memory, &mut caller, count_ptr, count)?;
    write_u32(&memory, &mut caller, buf_size_ptr, buf_size)?;
    Ok(ESUCCESS)
}

pub fn environ_get(
    mut caller: Caller<'_, WasiCtx>,
    environ_ptr: u32,
    environ_buf_ptr: u32,
) -> Result<u32, Error> {
    let env = {
        let bc = caller.data();
        bc.env.clone()
    };
    let memory = get_memory(&caller)?;
    let mut buf_offset = environ_buf_ptr;
    for (i, entry) in env.iter().enumerate() {
        let addr = match environ_ptr.checked_add(
            (i as u32)
                .checked_mul(4)
                .ok_or_else(|| Error::new("overflow"))?,
        ) {
            Some(a) => a,
            None => return Ok(EINVAL),
        };
        write_u32(&memory, &mut caller, addr, buf_offset)?;
        memory
            .write(&mut caller, buf_offset as usize, entry)
            .map_err(|_| Error::new("environ_get: write failed"))?;
        buf_offset = buf_offset
            .checked_add(entry.len() as u32)
            .ok_or_else(|| Error::new("overflow"))?;
    }
    Ok(ESUCCESS)
}

pub fn args_sizes_get(
    mut caller: Caller<'_, WasiCtx>,
    count_ptr: u32,
    buf_size_ptr: u32,
) -> Result<u32, Error> {
    let memory = get_memory(&caller)?;
    let (count, buf_size) = {
        let bc = caller.data();
        (
            bc.args.len() as u32,
            bc.args.iter().map(|a| a.len() as u32).sum::<u32>(),
        )
    };
    write_u32(&memory, &mut caller, count_ptr, count)?;
    write_u32(&memory, &mut caller, buf_size_ptr, buf_size)?;
    Ok(ESUCCESS)
}

pub fn args_get(
    mut caller: Caller<'_, WasiCtx>,
    argv_ptr: u32,
    argv_buf_ptr: u32,
) -> Result<u32, Error> {
    let args = {
        let bc = caller.data();
        bc.args.clone()
    };
    let memory = get_memory(&caller)?;
    let mut buf_offset = argv_buf_ptr;
    for (i, arg) in args.iter().enumerate() {
        let addr = match argv_ptr.checked_add(
            (i as u32)
                .checked_mul(4)
                .ok_or_else(|| Error::new("overflow"))?,
        ) {
            Some(a) => a,
            None => return Ok(EINVAL),
        };
        write_u32(&memory, &mut caller, addr, buf_offset)?;
        memory
            .write(&mut caller, buf_offset as usize, arg)
            .map_err(|_| Error::new("args_get: write failed"))?;
        buf_offset = buf_offset
            .checked_add(arg.len() as u32)
            .ok_or_else(|| Error::new("overflow"))?;
    }
    Ok(ESUCCESS)
}

pub fn clock_time_get(
    mut caller: Caller<'_, WasiCtx>,
    id: u32,
    _precision: u64,
    time_ptr: u32,
) -> Result<u32, Error> {
    let time = {
        let bc = caller.data();
        match id {
            CLOCK_MONOTONIC => (bc.get_monotonic_ns)(),
            CLOCK_REALTIME => return Ok(ENOTSUP),
            _ => return Ok(ENOTSUP),
        }
    };
    let memory = get_memory(&caller)?;
    write_u64(&memory, &mut caller, time_ptr, time)?;
    Ok(ESUCCESS)
}

pub fn random_get(
    mut caller: Caller<'_, WasiCtx>,
    buf_ptr: u32,
    buf_len: u32,
) -> Result<u32, Error> {
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (caller, buf_ptr, buf_len);
        return Ok(ENOTSUP);
    }

    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: CPUID leaf 1 is always supported on x86_64.
        let cpuid = core::arch::x86_64::__cpuid(1);
        if (cpuid.ecx & (1 << 30)) == 0 {
            return Ok(ENOTSUP);
        }

        let memory = get_memory(&caller)?;
        let mut offset = 0;
        while offset < buf_len {
            let chunk_len = (buf_len - offset).min(128) as usize;
            let mut temp_buf = [0u8; 128];
            let mut i = 0;
            while i < chunk_len {
                let mut val: u64 = 0;
                let mut retries = 10;
                let mut success = false;
                while retries > 0 {
                    if unsafe { core::arch::x86_64::_rdrand64_step(&mut val) } == 1 {
                        success = true;
                        break;
                    }
                    retries -= 1;
                    core::hint::spin_loop();
                }
                if !success {
                    return Err(Error::new("random_get: entropy exhausted"));
                }
                let end = (i + 8).min(chunk_len);
                temp_buf[i..end].copy_from_slice(&val.to_le_bytes()[..end - i]);
                i = end;
            }
            memory
                .write(
                    &mut caller,
                    (buf_ptr as usize) + (offset as usize),
                    &temp_buf[..chunk_len],
                )
                .map_err(|_| Error::new("random_get: write failed"))?;
            offset += chunk_len as u32;
        }
        Ok(ESUCCESS)
    }
}

// ── Fullerene custom host functions ──────────────────────────────

/// Return the compositor framebuffer dimensions without allocating a capture.
pub fn fullerene_screen_dimensions(
    mut caller: Caller<'_, WasiCtx>,
    width_ptr: u32,
    height_ptr: u32,
) -> Result<u32, Error> {
    let (width, height) = (caller.data().screen_dimensions)();
    let memory = get_memory(&caller)?;
    write_u32(&memory, &mut caller, width_ptr, width)?;
    write_u32(&memory, &mut caller, height_ptr, height)?;
    Ok(ESUCCESS)
}

/// Copy the compositor's clean desktop back buffer into WASM memory.
pub fn fullerene_capture_screen(
    mut caller: Caller<'_, WasiCtx>,
    pixels_ptr: u32,
    pixels_len: u32,
    width_ptr: u32,
    height_ptr: u32,
) -> Result<u32, Error> {
    let Some((width, height, pixels)) = (caller.data().capture_screen)() else {
        return Ok(ENOTSUP);
    };
    if pixels.len() > MAX_CAPTURE_RGBA_BYTES as usize || pixels.len() > pixels_len as usize {
        return Ok(EINVAL);
    }
    let memory = get_memory(&caller)?;
    memory
        .write(&mut caller, pixels_ptr as usize, &pixels)
        .map_err(|_| Error::new("capture_screen: memory write failed"))?;
    write_u32(&memory, &mut caller, width_ptr, width)?;
    write_u32(&memory, &mut caller, height_ptr, height)?;
    Ok(ESUCCESS)
}

/// Copy one bounded RGBA capture chunk into WASM memory. Unlike the legacy
/// whole-image callback, this never requires a multi-megabyte guest buffer.
pub fn fullerene_capture_screen_chunk(
    mut caller: Caller<'_, WasiCtx>,
    offset: u32,
    pixels_ptr: u32,
    pixels_len: u32,
    width_ptr: u32,
    height_ptr: u32,
) -> Result<u32, Error> {
    if pixels_len == 0 || pixels_len > MAX_CAPTURE_CHUNK_BYTES || offset % 4 != 0 {
        return Ok(EINVAL);
    }
    let mut pixels = alloc::vec![0u8; pixels_len as usize];
    let Some((width, height)) = (caller.data().capture_screen_chunk)(offset, &mut pixels) else {
        return Ok(ENOTSUP);
    };
    let memory = get_memory(&caller)?;
    memory
        .write(&mut caller, pixels_ptr as usize, &pixels)
        .map_err(|_| Error::new("capture_screen_chunk: memory write failed"))?;
    write_u32(&memory, &mut caller, width_ptr, width)?;
    write_u32(&memory, &mut caller, height_ptr, height)?;
    Ok(ESUCCESS)
}

/// Display decoded RGB pixels in a new window.
pub fn fullerene_show_image(
    caller: Caller<'_, WasiCtx>,
    width: u32,
    height: u32,
    pixels_ptr: u32,
    pixels_len: u32,
) -> Result<u32, Error> {
    if pixels_len > MAX_DISPLAY_RGB_BYTES {
        return Ok(EINVAL);
    }
    let memory = get_memory(&caller)?;
    let mut pixels = alloc::vec![0u8; pixels_len as usize];
    memory
        .read(&caller, pixels_ptr as usize, &mut pixels)
        .map_err(|_| Error::new("show_image: memory read failed"))?;
    let code = (caller.data().show_image)(width, height, &pixels);
    Ok(code as u32)
}

/// Display text in an editor-style window.
pub fn fullerene_show_text(
    caller: Caller<'_, WasiCtx>,
    title_ptr: u32,
    title_len: u32,
    text_ptr: u32,
    text_len: u32,
) -> Result<u32, Error> {
    if title_len > MAX_WINDOW_TITLE_BYTES || text_len > MAX_TEXT_BYTES {
        return Ok(EINVAL);
    }
    let memory = get_memory(&caller)?;
    let mut title_buf = alloc::vec![0u8; title_len as usize];
    let mut text_buf = alloc::vec![0u8; text_len as usize];
    memory
        .read(&caller, title_ptr as usize, &mut title_buf)
        .map_err(|_| Error::new("show_text: read title failed"))?;
    memory
        .read(&caller, text_ptr as usize, &mut text_buf)
        .map_err(|_| Error::new("show_text: read text failed"))?;
    let title = core::str::from_utf8(&title_buf).unwrap_or("Viewer");
    let text = core::str::from_utf8(&text_buf).unwrap_or("");
    let code = (caller.data().show_text)(title, text);
    Ok(code as u32)
}

/// Show an error dialog.
pub fn fullerene_show_error(
    caller: Caller<'_, WasiCtx>,
    title_ptr: u32,
    title_len: u32,
    msg_ptr: u32,
    msg_len: u32,
) -> Result<u32, Error> {
    if title_len > MAX_WINDOW_TITLE_BYTES || msg_len > MAX_ERROR_BYTES {
        return Ok(EINVAL);
    }
    let memory = get_memory(&caller)?;
    let mut title_buf = alloc::vec![0u8; title_len as usize];
    let mut msg_buf = alloc::vec![0u8; msg_len as usize];
    memory
        .read(&caller, title_ptr as usize, &mut title_buf)
        .map_err(|_| Error::new("show_error: read title failed"))?;
    memory
        .read(&caller, msg_ptr as usize, &mut msg_buf)
        .map_err(|_| Error::new("show_error: read msg failed"))?;
    let title = core::str::from_utf8(&title_buf).unwrap_or("Error");
    let msg = core::str::from_utf8(&msg_buf).unwrap_or("");
    let code = (caller.data().show_error)(title, msg);
    Ok(code as u32)
}

/// Create a persistent window, returning a handle that can be used with
/// `update_window`.  Returns -1 on failure.
pub fn fullerene_create_window(
    caller: Caller<'_, WasiCtx>,
    title_ptr: u32,
    title_len: u32,
    width: u32,
    height: u32,
) -> Result<u32, Error> {
    if title_len > MAX_WINDOW_TITLE_BYTES {
        return Ok(EINVAL);
    }
    let memory = get_memory(&caller)?;
    let mut title_buf = alloc::vec![0u8; title_len as usize];
    memory
        .read(&caller, title_ptr as usize, &mut title_buf)
        .map_err(|_| Error::new("create_window: read title failed"))?;
    let title = core::str::from_utf8(&title_buf).unwrap_or("Window");
    let id = (caller.data().create_window)(title, width, height);
    Ok(id as u32)
}

/// Update pixel contents of a window previously created with `create_window`.
pub fn fullerene_update_window(
    caller: Caller<'_, WasiCtx>,
    window_id: i32,
    width: u32,
    height: u32,
    pixels_ptr: u32,
    pixels_len: u32,
) -> Result<u32, Error> {
    if pixels_len > MAX_DISPLAY_RGB_BYTES {
        return Ok(EINVAL);
    }
    let memory = get_memory(&caller)?;
    let mut pixels = alloc::vec![0u8; pixels_len as usize];
    memory
        .read(&caller, pixels_ptr as usize, &mut pixels)
        .map_err(|_| Error::new("update_window: memory read failed"))?;
    let code = (caller.data().update_window)(window_id, width, height, &pixels);
    Ok(code as u32)
}

/// Close a window previously created with `create_window`.
pub fn fullerene_close_window(caller: Caller<'_, WasiCtx>, window_id: i32) -> Result<u32, Error> {
    let code = (caller.data().close_window)(window_id);
    Ok(code as u32)
}
