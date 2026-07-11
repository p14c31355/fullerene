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

pub const CLOCK_REALTIME: u32 = 0;
pub const CLOCK_MONOTONIC: u32 = 1;

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
    pub write_stdout: fn(&[u8]),
    pub write_stderr: fn(&[u8]),
    pub read_stdin: fn() -> Option<u8>,
    pub yield_now: fn(),
    pub read_entire_file: fn(&str) -> Result<Vec<u8>, &'static str>,
    pub read_directory: fn(&str) -> Result<Vec<(String, u8)>, &'static str>,
    pub get_monotonic_ns: fn() -> u64,
}

impl WasiCtx {
    pub fn new(
        args: &[&str],
        write_stdout: fn(&[u8]),
        write_stderr: fn(&[u8]),
        read_stdin: fn() -> Option<u8>,
        yield_now: fn(),
        read_entire_file: fn(&str) -> Result<Vec<u8>, &'static str>,
        read_directory: fn(&str) -> Result<Vec<(String, u8)>, &'static str>,
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
            read_directory,
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
        let base = match iovs_ptr.checked_add(i.checked_mul(8).ok_or_else(|| Error::new("overflow"))?) {
            Some(b) => b,
            None => return Ok(EINVAL),
        };
        let buf_ptr = read_u32(&memory, &caller, base)?;
        let buf_len = read_u32(&memory, &caller, base + 4)?;
        let mut offset = 0;
        let mut temp_buf = [0u8; 4096];
        while offset < buf_len {
            let chunk_len = (buf_len - offset).min(4096) as usize;
            memory
                .read(&caller, (buf_ptr + offset) as usize, &mut temp_buf[..chunk_len])
                .map_err(|_| Error::new("fd_write: read iov failed"))?;
            let ctx = caller.data();
            match fd {
                1 => (ctx.write_stdout)(&temp_buf[..chunk_len]),
                2 => (ctx.write_stderr)(&temp_buf[..chunk_len]),
                _ => return Ok(EBADF),
            }
            offset += chunk_len as u32;
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
    let mut total_read: u32 = 0;
    match fd {
        0 => {
            // Wait for at least one byte to be available.
            let first_byte = loop {
                match (caller.data().read_stdin)() {
                    Some(byte) => break byte,
                    None => (caller.data().yield_now)(),
                }
            };
            let mut temp_buf = [0u8; 4096];
            for i in 0..iovs_len {
                let base = match iovs_ptr.checked_add(i.checked_mul(8).ok_or_else(|| Error::new("overflow"))?) {
                    Some(b) => b,
                    None => return Ok(EINVAL),
                };
                let buf_ptr = read_u32(&memory, &caller, base)?;
                let buf_len = read_u32(&memory, &caller, base + 4)?;
                let mut iov_written: u32 = 0;
                while iov_written < buf_len {
                    let chunk_len = (buf_len - iov_written).min(4096) as usize;
                    let mut chunk_read = 0;
                    // Use the first byte from the wait loop if this is the first chunk.
                    if total_read == 0 && chunk_read == 0 {
                        temp_buf[0] = first_byte;
                        chunk_read = 1;
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
                        .write(&mut caller, (buf_ptr + iov_written) as usize, &temp_buf[..chunk_read])
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
                let base = iovs_ptr + i * 8;
                let buf_ptr = read_u32(&memory, &caller, base)?;
                let buf_len = read_u32(&memory, &caller, base + 4)?;
                let (to_read, current_offset) = {
                    let bc = caller.data();
                    match bc.fds.get(&fd) {
                        Some(WasiFd::File { data, offset }) => {
                            let available = data.len().saturating_sub(*offset);
                            ((buf_len as usize).min(available), *offset)
                        }
                        _ => return Ok(EBADF),
                    }
                };
                if to_read > 0 {
                    let chunk = {
                        let bc = caller.data();
                        match bc.fds.get(&fd) {
                            Some(WasiFd::File { data, .. }) => {
                                data[current_offset..current_offset + to_read].to_vec()
                            }
                            _ => return Ok(EBADF),
                        }
                    };
                    memory
                        .write(&mut caller, buf_ptr as usize, &chunk)
                        .map_err(|_| Error::new("fd_read: write data failed"))?;
                    let bc = caller.data_mut();
                    if let Some(WasiFd::File { offset: o, .. }) = bc.fds.get_mut(&fd) {
                        *o = current_offset + to_read;
                    }
                    total_read += to_read as u32;
                }
                if to_read < buf_len as usize {
                    break;
                }
            }
            write_u32(&memory, &mut caller, nread_ptr, total_read)?;
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
    if path_len > 1024 {
        return Ok(EINVAL);
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
    if path_len > 1024 {
        return Ok(EINVAL);
    }
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
    cookie: u64,
    bufused_ptr: u32,
) -> Result<u32, Error> {
    let entries = {
        let bc = caller.data();
        match bc.fds.get(&fd) {
            Some(WasiFd::PreopenedDir { path }) => {
                (bc.read_directory)(path).unwrap_or_default()
            }
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
            write_u64(&memory, &mut caller, buf_ptr, 1)?;
            write_u64(&memory, &mut caller, buf_ptr + 8, 0)?;
            write_u32(&memory, &mut caller, buf_ptr + 16, name.len() as u32)?;
            write_u8(&memory, &mut caller, buf_ptr + 20, FILETYPE_DIRECTORY)?;
            memory
                .write(&mut caller, (buf_ptr + 24) as usize, name)
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
        let off = buf_ptr + used;
        let next_cookie = entry_idx + 2;
        write_u64(&memory, &mut caller, off, next_cookie as u64)?;
        write_u64(&memory, &mut caller, off + 8, entry_idx as u64)?;
        write_u32(&memory, &mut caller, off + 16, name_bytes.len() as u32)?;
        write_u8(&memory, &mut caller, off + 20, filetype)?;
        memory
            .write(&mut caller, (off + 24) as usize, name_bytes)
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
        let addr = match environ_ptr.checked_add((i as u32).checked_mul(4).ok_or_else(|| Error::new("overflow"))?) {
            Some(a) => a,
            None => return Ok(EINVAL),
        };
        write_u32(&memory, &mut caller, addr, buf_offset)?;
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
        let addr = match argv_ptr.checked_add((i as u32).checked_mul(4).ok_or_else(|| Error::new("overflow"))?) {
            Some(a) => a,
            None => return Ok(EINVAL),
        };
        write_u32(&memory, &mut caller, addr, buf_offset)?;
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
    let mut temp_buf = [0u8; 128];
    let mut offset = 0;
    let memory = get_memory(&caller)?;
    while offset < buf_len {
        let chunk_len = (buf_len - offset).min(128) as usize;
        let mut i = 0;
        while i + 8 <= chunk_len {
            let mut val: u64 = 0;
            let mut success = false;
            #[cfg(target_arch = "x86_64")]
            {
                let mut retries = 10;
                while retries > 0 {
                    if unsafe { core::arch::x86_64::_rdrand64_step(&mut val) } == 1 {
                        success = true;
                        break;
                    }
                    retries -= 1;
                    core::hint::spin_loop();
                }
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                return Err(Error::new("random_get: entropy not available on this architecture"));
            }
            if !success {
                return Err(Error::new("random_get: entropy exhausted"));
            }
            temp_buf[i..i + 8].copy_from_slice(&val.to_le_bytes());
            i += 8;
        }
        if i < chunk_len {
            let mut val: u64 = 0;
            let mut success = false;
            #[cfg(target_arch = "x86_64")]
            {
                let mut retries = 10;
                while retries > 0 {
                    if unsafe { core::arch::x86_64::_rdrand64_step(&mut val) } == 1 {
                        success = true;
                        break;
                    }
                    retries -= 1;
                    core::hint::spin_loop();
                }
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                return Err(Error::new("random_get: entropy not available on this architecture"));
            }
            if !success {
                return Err(Error::new("random_get: entropy exhausted"));
            }
            temp_buf[i..chunk_len].copy_from_slice(&val.to_le_bytes()[..chunk_len - i]);
        }
        memory
            .write(&mut caller, (buf_ptr + offset) as usize, &temp_buf[..chunk_len])
            .map_err(|_| Error::new("random_get: write failed"))?;
        offset += chunk_len as u32;
    }
    Ok(ESUCCESS)
}
