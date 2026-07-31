use alloc::collections::BTreeMap;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::str;
use wasmi::{AsContext, Caller, Error, Memory};
use z264::{Decoder as H264Decoder, Frame as H264Frame};

// ── WASI errno ─────────────────────────────────────────────────────

pub const ESUCCESS: u32 = 0;
pub const EACCES: u32 = 2;
pub const EBADF: u32 = 8;
pub const EBUSY: u32 = 10;
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

pub type WriteBytes = fn(&[u8]);
pub type ReadStdin = fn() -> Option<u8>;
pub type FileSize = fn(&str) -> Result<u64, genome::FsError>;
pub type ReadFileRange = fn(&str, u64, usize) -> Result<Vec<u8>, genome::FsError>;
pub type ReadDirectory = fn(&str) -> Result<Vec<(String, u8)>, genome::FsError>;
pub type WriteFile = fn(&str, &[u8]) -> Result<(), genome::FsError>;
pub type WriteFileChunk = fn(&str, u64, &[u8], bool) -> Result<(), genome::FsError>;
pub type GetMonotonicNs = fn() -> u64;
pub type VideoClockNs = fn() -> u64;
pub type ScreenDimensions = fn() -> (u32, u32);
pub type CaptureScreen = fn() -> Option<(u32, u32, Vec<u8>)>;
pub type CaptureScreenChunk = fn(u32, &mut [u8]) -> Option<(u32, u32)>;
pub type ShowImage = fn(u32, u32, &[u8]) -> i32;
pub type ShowText = fn(&str, &str) -> i32;
pub type ShowError = fn(&str, &str) -> i32;
pub type CreateWindow = fn(&str, u32, u32) -> i32;
pub type UpdateWindow = fn(i32, u32, u32, &[u8]) -> i32;
pub type CloseWindow = fn(i32) -> i32;
pub type PlayPcm = fn(u32, u8, u8, &[u8]) -> i32;
/// Return accumulated native/kernel video timing in nanoseconds.
pub type VideoStageTiming = fn(u32) -> u64;

pub const VIDEO_STAGE_YUV_TO_RGB: u32 = 0;
pub const VIDEO_STAGE_SCALE: u32 = 1;
pub const VIDEO_STAGE_WINDOW_BUFFER_COPY: u32 = 2;
pub const VIDEO_STAGE_COMPOSITE: u32 = 3;
pub const VIDEO_STAGE_FRAMEBUFFER_FLUSH: u32 = 4;
pub const VIDEO_STAGE_RESET: u32 = u32::MAX;

pub struct WasiHost {
    pub write_stdout: WriteBytes,
    pub write_stderr: WriteBytes,
    pub read_stdin: ReadStdin,
    pub yield_now: fn(),
    pub wait_for_ns: fn(u64),
    pub file_size: FileSize,
    pub read_file_range: ReadFileRange,
    pub read_directory: ReadDirectory,
    pub write_file: WriteFile,
    pub write_file_chunk: WriteFileChunk,
    pub get_monotonic_ns: GetMonotonicNs,
    pub video_clock_ns: VideoClockNs,
    pub screen_dimensions: ScreenDimensions,
    pub capture_screen: CaptureScreen,
    pub capture_screen_chunk: CaptureScreenChunk,
    pub show_image: ShowImage,
    pub show_text: ShowText,
    pub show_error: ShowError,
    pub create_window: CreateWindow,
    pub update_window: UpdateWindow,
    pub close_window: CloseWindow,
    pub play_pcm: PlayPcm,
    pub video_stage_timing: VideoStageTiming,
}

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
// PPBUF-backed media reads are synchronous. Keep each refill short enough
// that playback does not disappear behind one long removable-media read.
const FILE_CACHE_BYTES: usize = 64 * 1024;
const MAX_DISPLAY_RGB_BYTES: u32 = 800 * 600 * 3;
const MAX_CAPTURE_RGBA_BYTES: u32 = 32 * 1024 * 1024;
const MAX_CAPTURE_CHUNK_BYTES: u32 = 256 * 1024;
const MAX_WINDOW_TITLE_BYTES: u32 = 1024;
const MAX_TEXT_BYTES: u32 = 512 * 1024;
const MAX_ERROR_BYTES: u32 = 64 * 1024;
const MAX_AUDIO_BYTES: u32 = 8 * 1024 * 1024;
const MAX_NATIVE_VIDEO_CONFIG_BYTES: u32 = 64 * 1024;
const MAX_NATIVE_VIDEO_SAMPLE_BYTES: u32 = 8 * 1024 * 1024;
const MAX_NATIVE_VIDEO_NALS: usize = 128;

/// Native H.264 state owned by one WASM invocation.
///
/// The viewer still owns MP4 demuxing and presentation timing. Compressed
/// samples cross the host boundary once, while parsing, decoding, YUV→RGB
/// conversion, and the temporary frame queue stay native. This is important
/// for wasmi: none of the expensive per-macroblock work is charged to the
/// interpreter or performed through WASM memory.
struct NativeVideo {
    decoder: H264Decoder,
    pending: VecDeque<H264Frame>,
    rgb: Vec<u8>,
    yuv_to_rgb_ns: u64,
    scale_ns: u64,
    host_update_ns: u64,
}

impl NativeVideo {
    fn open(config_annex_b: &[u8]) -> Result<Self, ()> {
        let mut decoder = H264Decoder::new();
        for nal in z264::nal::parse_annex_b(config_annex_b) {
            decoder.decode_nal(&nal).map_err(|_| ())?;
        }
        Ok(Self {
            decoder,
            pending: VecDeque::new(),
            rgb: Vec::new(),
            yuv_to_rgb_ns: 0,
            scale_ns: 0,
            host_update_ns: 0,
        })
    }

    fn decode_sample(&mut self, sample: &[u8], length_size: usize) -> Result<u32, ()> {
        let nals = z264::nal::parse_avcc(sample, length_size);
        if nals.len() > MAX_NATIVE_VIDEO_NALS {
            return Err(());
        }
        for nal in nals {
            if let Some(frame) = self.decoder.decode_nal(&nal).map_err(|_| ())? {
                self.pending.push_back(frame);
            }
        }
        Ok(self.pending.len() as u32)
    }

    fn flush(&mut self) -> u32 {
        if let Some(frame) = self.decoder.flush() {
            self.pending.push_back(frame);
        }
        self.pending.len() as u32
    }

    fn frame_info(&self) -> Option<(u32, u32)> {
        self.pending
            .front()
            .map(|frame| fit_video_dimensions(frame.width, frame.height))
    }

    fn discard(&mut self) -> Result<(), ()> {
        self.pending.pop_front().map(|_| ()).ok_or(())
    }

    fn present(
        &mut self,
        window_id: i32,
        update: UpdateWindow,
        now: GetMonotonicNs,
    ) -> Result<(u32, u32), ()> {
        let frame = self.pending.pop_front().ok_or(())?;
        let conversion_start = now();
        let (width, height, scaled) = yuv420_to_rgb(&frame, &mut self.rgb).ok_or(())?;
        let conversion_ns = now().saturating_sub(conversion_start);
        if scaled {
            self.scale_ns = self.scale_ns.saturating_add(conversion_ns);
        } else {
            self.yuv_to_rgb_ns = self.yuv_to_rgb_ns.saturating_add(conversion_ns);
        }
        if window_id >= 0 {
            let update_start = now();
            let code = update(window_id, width, height, &self.rgb);
            self.host_update_ns = self
                .host_update_ns
                .saturating_add(now().saturating_sub(update_start));
            if code != 0 {
                return Err(());
            }
        }
        Ok((width, height))
    }
}

fn fit_video_dimensions(width: u32, height: u32) -> (u32, u32) {
    if width <= 800 && height <= 600 {
        return (width, height);
    }
    if u64::from(width) * 600 <= u64::from(height) * 800 {
        (
            (u64::from(width) * 600 / u64::from(height)).max(1) as u32,
            600,
        )
    } else {
        (
            800,
            (u64::from(height) * 800 / u64::from(width)).max(1) as u32,
        )
    }
}

fn yuv420_to_rgb(frame: &H264Frame, rgb: &mut Vec<u8>) -> Option<(u32, u32, bool)> {
    let source_width = usize::try_from(frame.width).ok()?;
    let source_height = usize::try_from(frame.height).ok()?;
    if source_width == 0 || source_height == 0 {
        return None;
    }
    let (width, height) = fit_video_dimensions(frame.width, frame.height);
    let width = usize::try_from(width).ok()?;
    let height = usize::try_from(height).ok()?;
    let y_len = source_width.checked_mul(source_height)?;
    let uv_width = source_width.div_ceil(2);
    let uv_height = source_height.div_ceil(2);
    let uv_len = uv_width.checked_mul(uv_height)?;
    if frame.y.len() < y_len || frame.u.len() < uv_len || frame.v.len() < uv_len {
        return None;
    }
    let rgb_len = width.checked_mul(height)?.checked_mul(3)?;
    rgb.resize(rgb_len, 0);
    if width == source_width && height == source_height {
        for source_y in 0..source_height {
            let y_row = source_y * source_width;
            let uv_row = (source_y / 2) * uv_width;
            let rgb_row = source_y * width * 3;
            for source_x in 0..source_width {
                let yi = y_row + source_x;
                let ui = uv_row + source_x / 2;
                let dst = rgb_row + source_x * 3;
                let yv = frame.y[yi] as i32;
                let uv = frame.u[ui] as i32 - 128;
                let vv = frame.v[ui] as i32 - 128;
                rgb[dst] = (yv + (359 * vv) / 256).clamp(0, 255) as u8;
                rgb[dst + 1] = (yv - (88 * uv + 183 * vv) / 256).clamp(0, 255) as u8;
                rgb[dst + 2] = (yv + (454 * uv) / 256).clamp(0, 255) as u8;
            }
        }
    } else {
        for output_y in 0..height {
            let source_y = output_y * source_height / height;
            let uv_row = (source_y / 2) * uv_width;
            for output_x in 0..width {
                let source_x = output_x * source_width / width;
                let yi = source_y * source_width + source_x;
                let ui = uv_row + source_x / 2;
                let dst = (output_y * width + output_x) * 3;
                let yv = frame.y[yi] as i32;
                let uv = frame.u[ui] as i32 - 128;
                let vv = frame.v[ui] as i32 - 128;
                rgb[dst] = (yv + (359 * vv) / 256).clamp(0, 255) as u8;
                rgb[dst + 1] = (yv - (88 * uv + 183 * vv) / 256).clamp(0, 255) as u8;
                rgb[dst + 2] = (yv + (454 * uv) / 256).clamp(0, 255) as u8;
            }
        }
    }
    Some((
        width as u32,
        height as u32,
        width != source_width || height != source_height,
    ))
}

pub struct WasiCtx {
    pub exit_code: Option<u32>,
    pub args: Vec<Vec<u8>>,
    pub env: Vec<Vec<u8>>,
    pub fds: BTreeMap<u32, WasiFd>,
    pub next_fd: u32,
    /// Bounded compute-budget extensions used by long-running MP4 decode.
    /// Fuel is replenished only at an explicit host yield point, never while
    /// a pure WASM computation is running.
    pub fuel_refills_left: u16,
    pub fuel_refill_amount: u64,
    pub host: WasiHost,
    native_video: Option<NativeVideo>,
    video_sample_scratch: Vec<u8>,
}

impl WasiCtx {
    pub fn new(args: &[&str], host: WasiHost) -> Self {
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
            fuel_refills_left: 0,
            fuel_refill_amount: 0,
            host,
            native_video: None,
            video_sample_scratch: Vec::new(),
        }
    }
}

impl core::ops::Deref for WasiCtx {
    type Target = WasiHost;

    fn deref(&self) -> &Self::Target {
        &self.host
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
        FsError::Busy => EBUSY,
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

fn pixel_bytes(width: u32, height: u32, channels: u32) -> Option<u32> {
    u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(u64::from(channels))
        .and_then(|bytes| u32::try_from(bytes).ok())
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
                    for slot in temp_buf.iter_mut().take(chunk_len).skip(chunk_read) {
                        match (caller.data().read_stdin)() {
                            Some(byte) => {
                                *slot = byte;
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
                        let fetch_len = (size - offset).min(FILE_CACHE_BYTES as u64) as usize;
                        let read_file_range = caller.data().read_file_range;
                        let fetched = match read_file_range(&path, offset, fetch_len) {
                            Ok(bytes) => bytes,
                            Err(error) => {
                                let errno = map_fs_error(&error);
                                let message = alloc::format!(
                                    "[WASI-DIAG] fd_read range error path={} offset={} request={} fs={:?} errno={}\n",
                                    path,
                                    offset,
                                    fetch_len,
                                    error,
                                    errno
                                );
                                (caller.data().write_stderr)(message.as_bytes());
                                return Ok(errno);
                            }
                        };
                        if fetched.is_empty() {
                            let message = alloc::format!(
                                "[WASI-DIAG] fd_read short EOF path={} offset={} size={} request={} errno={}\n",
                                path,
                                offset,
                                size,
                                fetch_len,
                                EIO
                            );
                            (caller.data().write_stderr)(message.as_bytes());
                            return Ok(EIO);
                        }
                        if fetched.len() > fetch_len {
                            let message = alloc::format!(
                                "[WASI-DIAG] fd_read invalid host range path={} offset={} request={} returned={} errno={}\n",
                                path,
                                offset,
                                fetch_len,
                                fetched.len(),
                                EIO
                            );
                            (caller.data().write_stderr)(message.as_bytes());
                            return Ok(EIO);
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
        WHENCE_CUR => i64::try_from(current_offset)
            .ok()
            .and_then(|current| current.checked_add(offset)),
        WHENCE_END => i64::try_from(file_len)
            .ok()
            .and_then(|end| end.checked_add(offset)),
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
    write_u64(&memory, &mut caller, newoffset_ptr, new_offset)?;
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

#[allow(clippy::too_many_arguments)]
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
            write_u64(&memory, &mut caller, off_next, 1)?;
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
    for (entry_idx, (name, filetype)) in entries.iter().enumerate().skip(start_entry) {
        let name_bytes = name.as_bytes();
        let entry_size = 24u32.saturating_add(name_bytes.len() as u32);
        if used.saturating_add(entry_size) > buf_len {
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
        write_u64(&memory, &mut caller, off_next, next_cookie as u64)?;
        write_u32(&memory, &mut caller, off_namelen, name_bytes.len() as u32)?;
        write_u8(&memory, &mut caller, off_type, *filetype)?;
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

/// Cooperative scheduling point for synchronous WASM applications such as
/// the MP4 player. The host callback also lets the compositor repaint while
/// the application remains inside one WASM invocation.
pub fn sched_yield(caller: Caller<'_, WasiCtx>) -> Result<u32, Error> {
    (caller.data().yield_now)();
    Ok(ESUCCESS)
}

pub fn fullerene_wait_for_ns(
    mut caller: Caller<'_, WasiCtx>,
    duration_ns: u64,
) -> Result<u32, Error> {
    (caller.data().wait_for_ns)(duration_ns);
    // A synchronous MP4 viewer yields before each NAL. Replenish a bounded
    // amount of fuel at that boundary so a valid long video is not limited to
    // one global 1e9-instruction budget. Do not call set_fuel at every yield:
    // the viewer also yields with wait_for_ns(0) every few samples, and the
    // previous unconditional refill paid the host-side fuel update cost
    // thousands of times even while plenty of fuel remained. A malformed NAL
    // still traps once its current chunk is exhausted, because no host
    // callback can run inside decode_nal itself.
    let refill_amount = caller.data().fuel_refill_amount;
    if caller.data().fuel_refills_left != 0 && refill_amount != 0 {
        let current = caller.get_fuel().unwrap_or(0);
        if current < refill_amount {
            let should_refill = {
                let ctx = caller.data_mut();
                if ctx.fuel_refills_left == 0 {
                    false
                } else {
                    ctx.fuel_refills_left -= 1;
                    true
                }
            };
            if should_refill {
                let _ = caller.set_fuel(current.saturating_add(refill_amount));
            }
        }
    }
    Ok(ESUCCESS)
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
    let Some(expected_len) = pixel_bytes(width, height, 4) else {
        return Ok(EINVAL);
    };
    if expected_len > MAX_CAPTURE_RGBA_BYTES
        || pixels.len() != expected_len as usize
        || pixels.len() > pixels_len as usize
    {
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

/// Write one bounded output chunk directly to the host VFS.  Streaming
/// writers use this instead of WASI's close-time write buffer, which would
/// otherwise retain the complete screenshot in the guest heap.
pub fn fullerene_write_file_chunk(
    caller: Caller<'_, WasiCtx>,
    path_ptr: u32,
    path_len: u32,
    offset: u64,
    data_ptr: u32,
    data_len: u32,
    replace: u32,
) -> Result<u32, Error> {
    let end = offset.saturating_add(data_len as u64);
    if path_len == 0 || path_len > 4096 || data_len > 512 * 1024 || end > 64 * 1024 * 1024 {
        return Ok(EINVAL);
    }
    let memory = get_memory(&caller)?;
    let mut path_buf = vec![0u8; path_len as usize];
    memory
        .read(&caller, path_ptr as usize, &mut path_buf)
        .map_err(|_| Error::new("write_file_chunk: read path failed"))?;
    let mut data = vec![0u8; data_len as usize];
    memory
        .read(&caller, data_ptr as usize, &mut data)
        .map_err(|_| Error::new("write_file_chunk: read data failed"))?;
    let Ok(path) = str::from_utf8(&path_buf) else {
        return Ok(EINVAL);
    };
    let path = path.trim_end_matches('\0');
    if path.is_empty() {
        return Ok(EINVAL);
    }
    let result = (caller.data().write_file_chunk)(path, offset, &data, replace != 0);
    Ok(match result {
        Ok(()) => ESUCCESS,
        Err(error) => map_fs_error(&error),
    })
}

/// Display decoded RGB pixels in a new window.
pub fn fullerene_show_image(
    caller: Caller<'_, WasiCtx>,
    width: u32,
    height: u32,
    pixels_ptr: u32,
    pixels_len: u32,
) -> Result<u32, Error> {
    let Some(expected_len) = pixel_bytes(width, height, 3) else {
        return Ok(EINVAL);
    };
    if expected_len > MAX_DISPLAY_RGB_BYTES || pixels_len != expected_len {
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
    let Some(expected_len) = pixel_bytes(width, height, 3) else {
        return Ok(EINVAL);
    };
    if expected_len > MAX_DISPLAY_RGB_BYTES || pixels_len != expected_len {
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

/// Initialize the native z264 service with the Annex B SPS/PPS stream made
/// from the MP4 avcC record by the viewer.
pub fn fullerene_video_open(
    mut caller: Caller<'_, WasiCtx>,
    config_ptr: u32,
    config_len: u32,
) -> Result<u32, Error> {
    if config_len == 0 || config_len > MAX_NATIVE_VIDEO_CONFIG_BYTES {
        return Ok(EINVAL);
    }
    let memory = get_memory(&caller)?;
    let mut config = vec![0u8; config_len as usize];
    memory
        .read(&caller, config_ptr as usize, &mut config)
        .map_err(|_| Error::new("video_open: memory read failed"))?;
    let video = NativeVideo::open(&config)
        .map_err(|_| Error::new("video_open: invalid H.264 configuration"))?;
    (caller.data().video_stage_timing)(VIDEO_STAGE_RESET);
    caller.data_mut().native_video = Some(video);
    Ok(ESUCCESS)
}

/// Submit one length-prefixed MP4 sample to native z264. The return value is
/// the number of decoded frames currently waiting to be consumed.
pub fn fullerene_video_decode_sample(
    mut caller: Caller<'_, WasiCtx>,
    sample_ptr: u32,
    sample_len: u32,
    length_size: u32,
) -> Result<u32, Error> {
    if sample_len > MAX_NATIVE_VIDEO_SAMPLE_BYTES || !matches!(length_size, 1 | 2 | 4) {
        return Ok(EINVAL);
    }
    let memory = get_memory(&caller)?;
    // Reuse the host-side sample buffer across MP4 samples. Allocating a new
    // Vec for all 6572 Bad Apple samples adds allocator churn without helping
    // the decoder; the sample is consumed before the next host call returns.
    let mut sample = core::mem::take(&mut caller.data_mut().video_sample_scratch);
    sample.resize(sample_len as usize, 0);
    memory
        .read(&caller, sample_ptr as usize, &mut sample)
        .map_err(|_| Error::new("video_decode_sample: memory read failed"))?;
    let result = {
        let Some(video) = caller.data_mut().native_video.as_mut() else {
            caller.data_mut().video_sample_scratch = sample;
            return Ok(EINVAL);
        };
        video
            .decode_sample(&sample, length_size as usize)
            .unwrap_or(u32::MAX)
    };
    caller.data_mut().video_sample_scratch = sample;
    Ok(result)
}

/// Return the dimensions of the oldest pending decoded frame, packed as
/// `(width << 32) | height`.
pub fn fullerene_video_frame_info(caller: Caller<'_, WasiCtx>) -> Result<u64, Error> {
    let Some(video) = caller.data().native_video.as_ref() else {
        return Ok(0);
    };
    Ok(video
        .frame_info()
        .map(|(width, height)| (u64::from(width) << 32) | u64::from(height))
        .unwrap_or(0))
}

/// Convert and optionally present the oldest pending native frame. A
/// negative window id performs conversion only, which is used by the decode+
/// convert benchmark without allocating a WASM RGB buffer.
pub fn fullerene_video_present(
    mut caller: Caller<'_, WasiCtx>,
    window_id: i32,
) -> Result<u32, Error> {
    let update = caller.data().update_window;
    let now = caller.data().video_clock_ns;
    let Some(video) = caller.data_mut().native_video.as_mut() else {
        return Ok(EINVAL);
    };
    match video.present(window_id, update, now) {
        Ok(_) => Ok(ESUCCESS),
        Err(()) => Ok(EIO),
    }
}

/// Return accumulated timing for one video pipeline stage in nanoseconds.
///
/// Stages 0 and 1 are measured by the native video service. Stage 2 is
/// supplied by the kernel when available and falls back to the host callback
/// duration in portable/native benchmarks. Stages 3 and 4 are kernel-owned.
pub fn fullerene_video_stage_timing(caller: Caller<'_, WasiCtx>, stage: u32) -> Result<u64, Error> {
    let native = caller.data().native_video.as_ref();
    let native_value = native.map(|video| match stage {
        VIDEO_STAGE_YUV_TO_RGB => video.yuv_to_rgb_ns,
        VIDEO_STAGE_SCALE => video.scale_ns,
        VIDEO_STAGE_WINDOW_BUFFER_COPY => video.host_update_ns,
        _ => 0,
    });
    if let Some(value) = native_value
        && (stage == VIDEO_STAGE_YUV_TO_RGB
            || stage == VIDEO_STAGE_SCALE
            || (stage == VIDEO_STAGE_WINDOW_BUFFER_COPY
                && (caller.data().video_stage_timing)(stage) == 0))
    {
        return Ok(value);
    }
    Ok((caller.data().video_stage_timing)(stage))
}

/// Discard one decoded frame without converting it. This keeps decode-only
/// benchmarks bounded instead of retaining every frame of a long movie.
pub fn fullerene_video_discard(mut caller: Caller<'_, WasiCtx>) -> Result<u32, Error> {
    let Some(video) = caller.data_mut().native_video.as_mut() else {
        return Ok(EINVAL);
    };
    match video.discard() {
        Ok(()) => Ok(ESUCCESS),
        Err(()) => Ok(EIO),
    }
}

pub fn fullerene_video_flush(mut caller: Caller<'_, WasiCtx>) -> Result<u32, Error> {
    let Some(video) = caller.data_mut().native_video.as_mut() else {
        return Ok(EINVAL);
    };
    Ok(video.flush())
}

pub fn fullerene_video_close(mut caller: Caller<'_, WasiCtx>) -> Result<u32, Error> {
    caller.data_mut().native_video = None;
    Ok(ESUCCESS)
}

/// Close a window previously created with `create_window`.
pub fn fullerene_close_window(caller: Caller<'_, WasiCtx>, window_id: i32) -> Result<u32, Error> {
    let code = (caller.data().close_window)(window_id);
    Ok(code as u32)
}

/// Submit a bounded raw PCM buffer to the kernel audio backend.
pub fn fullerene_play_pcm(
    caller: Caller<'_, WasiCtx>,
    sample_rate: u32,
    channels: u32,
    bits_per_sample: u32,
    data_ptr: u32,
    data_len: u32,
) -> Result<u32, Error> {
    if data_len > MAX_AUDIO_BYTES
        || sample_rate == 0
        || channels == 0
        || bits_per_sample == 0
        || channels > u8::MAX as u32
        || bits_per_sample > u8::MAX as u32
    {
        return Ok(EINVAL);
    }
    let memory = get_memory(&caller)?;
    let mut data = alloc::vec![0u8; data_len as usize];
    memory
        .read(&caller, data_ptr as usize, &mut data)
        .map_err(|_| Error::new("play_pcm: memory read failed"))?;
    let code = (caller.data().play_pcm)(sample_rate, channels as u8, bits_per_sample as u8, &data);
    Ok(code as u32)
}
