use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::str;
use wasmi::{AsContext, Caller, Error, Memory};

// ── WASI errno ─────────────────────────────────────────────────────

pub const ESUCCESS: u32 = 0;
pub const EBADF: u32 = 8;
pub const EINVAL: u32 = 28;
pub const ENOENT: u32 = 44;
pub const ENOTSUP: u32 = 58;

// ── WASI file types ───────────────────────────────────────────────

pub const FILETYPE_DIRECTORY: u8 = 3;
pub const FILETYPE_REGULAR_FILE: u8 = 4;

// ── WASI whence ───────────────────────────────────────────────────

pub const WHENCE_SET: u32 = 0;
pub const WHENCE_CUR: u32 = 1;
pub const WHENCE_END: u32 = 2;

// ── WASI clock ids ────────────────────────────────────────────────

pub const CLOCK_MONOTONIC: u32 = 0;
pub const CLOCK_REALTIME: u32 = 1;

// ── FD table entry ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum WasiFd {
    Stdin,
    Stdout,
    Stderr,
    PreopenedDir { path: String },
    File { data: Vec<u8>, offset: usize },
}

// ── WASI context ─────────────────────────────────────────────────

pub struct WasiCtx {
    pub exit_code: Option<u32>,
    pub args: Vec<Vec<u8>>,
    pub env: Vec<Vec<u8>>,
    pub fds: BTreeMap<u32, WasiFd>,
    pub next_fd: u32,
    pub write_stdout: fn(&str),
    pub write_stderr: fn(&str),
    pub read_stdin: fn() -> Option<u8>,
    pub yield_now: fn(),
    pub read_entire_file: fn(&str) -> Result<Vec<u8>, &'static str>,
    pub get_monotonic_ns: fn() -> u64,
}

impl WasiCtx {
    pub fn new(
        args: &[&str],
        write_stdout: fn(&str),
        write_stderr: fn(&str),
        read_stdin: fn() -> Option<u8>,
        yield_now: fn(),
        read_entire_file: fn(&str) -> Result<Vec<u8>, &'static str>,
        get_monotonic_ns: fn() -> u64,
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
        fds.insert(3, WasiFd::PreopenedDir { path: String::from("/") });
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
            read_entire_file,
            get_monotonic_ns,
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

fn write_u32(memory: &Memory, ctx: impl wasmi::AsContextMut, addr: u32, val: u32) -> Result<(), Error> {
    let buf = val.to_le_bytes();
    memory
        .write(ctx, addr as usize, &buf)
        .map_err(|_| Error::new("memory write failed"))
}

fn write_u64(memory: &Memory, ctx: impl wasmi::AsContextMut, addr: u32, val: u64) -> Result<(), Error> {
    let buf = val.to_le_bytes();
    memory
        .write(ctx, addr as usize, &buf)
        .map_err(|_| Error::new("memory write failed"))
}

fn write_u8(memory: &Memory, ctx: impl wasmi::AsContextMut, addr: u32, val: u8) -> Result<(), Error> {
    let buf = [val];
    memory
        .write(ctx, addr as usize, &buf)
        .map_err(|_| Error::new("memory write failed"))
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
        let base = iovs_ptr + i * 8;
        let buf_ptr = read_u32(&memory, &caller, base)?;
        let buf_len = read_u32(&memory, &caller, base + 4)?;
        let mut chunk = vec![0u8; buf_len as usize];
        memory
            .read(&caller, buf_ptr as usize, &mut chunk)
            .map_err(|_| Error::new("fd_write: read iov failed"))?;
        let s = str::from_utf8(&chunk).unwrap_or("");
        let ctx = caller.data();
        match fd {
            1 => (ctx.write_stdout)(s),
            2 => (ctx.write_stderr)(s),
            _ => return Ok(EBADF),
        }
        total += buf_len;
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
        return Ok(ESUCCESS);
    }
    let memory = get_memory(&caller)?;
    let buf_ptr = read_u32(&memory, &caller, iovs_ptr)?;
    let buf_len = read_u32(&memory, &caller, iovs_ptr + 4)?;
    match fd {
        0 => {
            loop {
                let has_data = { (caller.data().read_stdin)().is_some() };
                if has_data {
                    break;
                }
                (caller.data().yield_now)();
            }
            let mut buf = vec![0u8; buf_len as usize];
            let mut read_count: u32 = 0;
            for _ in 0..buf_len {
                match (caller.data().read_stdin)() {
                    Some(byte) => {
                        buf[read_count as usize] = byte;
                        read_count += 1;
                    }
                    None => break,
                }
            }
            memory
                .write(&mut caller, buf_ptr as usize, &buf[..read_count as usize])
                .map_err(|_| Error::new("fd_read: write failed"))?;
            write_u32(&memory, &mut caller, nread_ptr, read_count)?;
            Ok(ESUCCESS)
        }
        _ => {
            let to_read = {
                let bc = caller.data();
                match bc.fds.get(&fd) {
                    Some(WasiFd::File { data, offset }) => {
                        (buf_len as usize).min(data.len().saturating_sub(*offset))
                    }
                    _ => return Ok(EBADF),
                }
            };
            if to_read > 0 {
                let chunk = {
                    let bc = caller.data();
                    match bc.fds.get(&fd) {
                        Some(WasiFd::File { data, offset }) => {
                            data[*offset..*offset + to_read].to_vec()
                        }
                        _ => return Ok(EBADF),
                    }
                };
                memory
                    .write(&mut caller, buf_ptr as usize, &chunk)
                    .map_err(|_| Error::new("fd_read: write data failed"))?;
                let bc = caller.data_mut();
                if let Some(WasiFd::File { offset: o, .. }) = bc.fds.get_mut(&fd) {
                    *o += to_read;
                }
            }
            let memory = get_memory(&caller)?;
            write_u32(&memory, &mut caller, nread_ptr, to_read as u32)?;
            Ok(ESUCCESS)
        }
    }
}

pub fn fd_close(mut caller: Caller<'_, WasiCtx>, fd: u32) -> Result<u32, Error> {
    if fd <= 3 {
        return Ok(ENOTSUP);
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
            Some(WasiFd::File { data, .. }) => data.len(),
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
    let new_offset: i64 = match whence {
        WHENCE_SET => offset,
        WHENCE_CUR => current_offset as i64 + offset,
        WHENCE_END => file_len as i64 + offset,
        _ => return Ok(EINVAL),
    };
    if new_offset < 0 {
        return Ok(EINVAL);
    }
    let new_offset = (new_offset as usize).min(file_len);
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

pub fn fd_prestat_get(
    mut caller: Caller<'_, WasiCtx>,
    fd: u32,
    buf: u32,
) -> Result<u32, Error> {
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
    write_u8(&memory, &mut caller, buf, 0)?;
    write_u32(&memory, &mut caller, buf + 4, name_len)?;
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
        .write(&mut caller, path_ptr as usize, &path.as_bytes()[..len as usize])
        .map_err(|_| Error::new("fd_prestat_dir_name: write failed"))?;
    Ok(ESUCCESS)
}

pub fn path_open(
    mut caller: Caller<'_, WasiCtx>,
    fd: u32,
    _dirflags: u32,
    path_ptr: u32,
    path_len: u32,
    _oflags: u32,
    _fs_rights_base: u64,
    _fs_rights_inheriting: u64,
    _fdflags: u32,
    result_fd_ptr: u32,
) -> Result<u32, Error> {
    if fd != 3 {
        return Ok(EBADF);
    }
    let memory = get_memory(&caller)?;
    let mut path_buf = vec![0u8; path_len as usize];
    memory
        .read(&caller, path_ptr as usize, &mut path_buf)
        .map_err(|_| Error::new("path_open: read path failed"))?;
    let path_str = str::from_utf8(&path_buf).map_err(|_| Error::new("path_open: invalid utf-8"))?;
    let clean = path_str.trim_matches('\0').trim_start_matches('/');
    let full_path = if clean.is_empty() { String::from("/") } else { alloc::format!("/{}", clean) };
    let data = {
        let bc = caller.data();
        (bc.read_entire_file)(&full_path)
    };
    match data {
        Ok(bytes) => {
            let new_fd = {
                let bc = caller.data_mut();
                let fd = bc.next_fd;
                bc.next_fd += 1;
                bc.fds.insert(fd, WasiFd::File { data: bytes, offset: 0 });
                fd
            };
            write_u32(&memory, &mut caller, result_fd_ptr, new_fd)?;
            Ok(ESUCCESS)
        }
        Err(_) => Ok(ENOENT),
    }
}

pub fn path_filestat_get(
    mut caller: Caller<'_, WasiCtx>,
    _fd: u32,
    _flags: u32,
    path_ptr: u32,
    path_len: u32,
    buf_ptr: u32,
) -> Result<u32, Error> {
    let memory = get_memory(&caller)?;
    let mut path_buf = vec![0u8; path_len as usize];
    memory
        .read(&caller, path_ptr as usize, &mut path_buf)
        .map_err(|_| Error::new("path_filestat_get: read path failed"))?;
    let path_str = str::from_utf8(&path_buf).unwrap_or("");
    let clean = path_str.trim_matches('\0').trim_start_matches('/');
    let full_path = if clean.is_empty() { String::from("/") } else { alloc::format!("/{}", clean) };
    let size = {
        let bc = caller.data();
        match (bc.read_entire_file)(&full_path) {
            Ok(d) => d.len() as u64,
            Err(_) => return Ok(ENOENT),
        }
    };
    write_u64(&memory, &mut caller, buf_ptr, 0)?;
    write_u64(&memory, &mut caller, buf_ptr + 8, 1)?;
    write_u8(&memory, &mut caller, buf_ptr + 16, FILETYPE_REGULAR_FILE)?;
    write_u64(&memory, &mut caller, buf_ptr + 24, 1)?;
    write_u64(&memory, &mut caller, buf_ptr + 32, size)?;
    write_u64(&memory, &mut caller, buf_ptr + 40, 0)?;
    write_u64(&memory, &mut caller, buf_ptr + 48, 0)?;
    write_u64(&memory, &mut caller, buf_ptr + 56, 0)?;
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
            Some(WasiFd::File { data, .. }) => (FILETYPE_REGULAR_FILE, data.len() as u64),
            Some(WasiFd::PreopenedDir { .. }) => (FILETYPE_DIRECTORY, 0u64),
            _ => return Ok(EBADF),
        }
    };
    let memory = get_memory(&caller)?;
    write_u64(&memory, &mut caller, buf_ptr, 0)?;
    write_u64(&memory, &mut caller, buf_ptr + 8, fd as u64)?;
    write_u8(&memory, &mut caller, buf_ptr + 16, filetype)?;
    write_u64(&memory, &mut caller, buf_ptr + 24, 1)?;
    write_u64(&memory, &mut caller, buf_ptr + 32, size)?;
    write_u64(&memory, &mut caller, buf_ptr + 40, 0)?;
    write_u64(&memory, &mut caller, buf_ptr + 48, 0)?;
    write_u64(&memory, &mut caller, buf_ptr + 56, 0)?;
    Ok(ESUCCESS)
}

pub fn fd_readdir(
    mut caller: Caller<'_, WasiCtx>,
    fd: u32,
    buf_ptr: u32,
    buf_len: u32,
    _cookie: u64,
    bufused_ptr: u32,
) -> Result<u32, Error> {
    let path = {
        let bc = caller.data();
        match bc.fds.get(&fd) {
            Some(WasiFd::PreopenedDir { path }) => path.clone(),
            _ => return Ok(EBADF),
        }
    };
    let entries = vec![String::from("."), path];
    let memory = get_memory(&caller)?;
    let mut used: u32 = 0;
    for (i, entry) in entries.iter().enumerate() {
        let name = entry.as_bytes();
        let entry_size = 24 + name.len() as u32;
        if used + entry_size > buf_len {
            break;
        }
        let off = buf_ptr + used;
        write_u64(&memory, &mut caller, off, (i + 1) as u64)?;
        write_u64(&memory, &mut caller, off + 8, i as u64)?;
        write_u32(&memory, &mut caller, off + 16, name.len() as u32)?;
        write_u8(&memory, &mut caller, off + 20, FILETYPE_DIRECTORY)?;
        memory
            .write(&mut caller, (off + 24) as usize, name)
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
        (bc.env.len() as u32, bc.env.iter().map(|e| e.len() as u32).sum::<u32>())
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
        write_u32(&memory, &mut caller, environ_ptr + (i as u32) * 4, buf_offset)?;
        memory
            .write(&mut caller, buf_offset as usize, entry)
            .map_err(|_| Error::new("environ_get: write failed"))?;
        buf_offset += entry.len() as u32;
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
        (bc.args.len() as u32, bc.args.iter().map(|a| a.len() as u32).sum::<u32>())
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
        write_u32(&memory, &mut caller, argv_ptr + (i as u32) * 4, buf_offset)?;
        memory
            .write(&mut caller, buf_offset as usize, arg)
            .map_err(|_| Error::new("args_get: write failed"))?;
        buf_offset += arg.len() as u32;
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
            CLOCK_MONOTONIC | CLOCK_REALTIME => (bc.get_monotonic_ns)(),
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
    let mut buf = vec![0u8; buf_len as usize];
    let mut i = 0;
    while i + 8 <= buf_len as usize {
        let mut val: u64 = 0;
        unsafe { core::arch::x86_64::_rdrand64_step(&mut val); }
        buf[i..i + 8].copy_from_slice(&val.to_le_bytes());
        i += 8;
    }
    if i < buf_len as usize {
        let mut val: u64 = 0;
        unsafe { core::arch::x86_64::_rdrand64_step(&mut val); }
        buf[i..].copy_from_slice(&val.to_le_bytes()[..buf_len as usize - i]);
    }
    let memory = get_memory(&caller)?;
    memory
        .write(&mut caller, buf_ptr as usize, &buf)
        .map_err(|_| Error::new("random_get: write failed"))?;
    Ok(ESUCCESS)
}
