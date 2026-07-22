//! Media file viewers — BMP, PNG, WAV, MP3, tar content display.
//!
//! Each viewer reads file data via `vfs_read`, parses the format, and
//! creates a window with the content rendered into the surface.

use crate::{GLYPH_H, HEAP_EXTEND_RESERVE, RUNTIME_CONTEXT, RuntimeState};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use petroleum::PageBuf;

const MAX_IMG_W: u32 = 1920;
const MAX_IMG_H: u32 = 1080;
const GLYPH_SIZE: u32 = 8;

/// Log a status message to the taskbar debug area + serial.
macro_rules! log_status {
    ($label:expr $(, $arg:expr)*) => {{
        #[cfg(not(test))]
        {
            let msg = alloc::format!($label $(, $arg)*);
            let s = petroleum::heap_stats();
            nitrogen::debug::print(
                "viewers",
                &alloc::format!("{} free={}K", msg, s.free / 1024),
            );
            petroleum::serial::serial_log(format_args!(
                "[viewers] {}  heap_free={} used={} total={}\n",
                msg, s.free, s.used, s.total,
            ));
        }
    }};
}

fn show_text_window(rt: &mut RuntimeState, title: &str, msg: &str, cols: u32, bg: u32, fg: u32) {
    let rows = (msg.lines().count() as u32).min(40) + 3;
    let id =
        rt.desktop
            .wm
            .create_titled_window(100, 60, cols * GLYPH_SIZE, rows * GLYPH_H, bg, title);
    if let Some(w) = rt.desktop.wm.windows_mut().iter_mut().find(|w| w.id == id) {
        let _ = crate::menu_actions::render_text_into_surface(&mut w.surface, msg, cols, fg, bg);
    }
    rt.desktop.wm.raise_to_top(id);
    rt.frame_due = true;
}

pub(crate) fn show_error(rt: &mut RuntimeState, title: &str, msg: &str) {
    show_text_window(rt, title, msg, 50, 0x1a1a0d, 0xFFCCCC);
}

// ── BMP viewer (tinybmp) ─────────────────────────────────────

#[cfg(feature = "tinybmp")]
pub fn open_bmp_data(rt: &mut RuntimeState, data: &[u8], _name: &str) {
    log_status!("BMP before decode");
    let bmp = match tinybmp::RawBmp::from_slice(data) {
        Ok(b) => b,
        Err(_) => {
            show_error(rt, "BMP Error", "Parse failed");
            return;
        }
    };
    log_status!("BMP after parse");
    if !matches!(
        bmp.header().bpp,
        tinybmp::Bpp::Bits24 | tinybmp::Bpp::Bits32
    ) {
        show_error(rt, "BMP Error", "Only 24-bit and 32-bit BMPs are supported");
        return;
    }
    let w = bmp.header().image_size.width;
    let h = bmp.header().image_size.height;
    if w > MAX_IMG_W || h > MAX_IMG_H {
        show_error(rt, "BMP Error", &format!("Image too large: {}x{}", w, h));
        return;
    }
    let win_w = w.min(800).max(160);
    let win_h = h.min(600).max(120);
    let id = rt
        .desktop
        .wm
        .create_titled_window(120, 80, win_w, win_h, 0x000000, "Image Viewer");
    if let Some(win) = rt.desktop.wm.windows_mut().iter_mut().find(|w| w.id == id) {
        for pixel in bmp.pixels() {
            let x = pixel.position.x;
            let y = pixel.position.y;
            if x >= 0 && y >= 0 {
                let ux = x as u32;
                let uy = y as u32;
                if ux < win_w && uy < win_h {
                    win.surface.set_pixel(ux, uy, pixel.color);
                }
            }
        }
        rt.desktop.invalidate_window(id);
    }
    rt.desktop.wm.raise_to_top(id);
    rt.frame_due = true;
}

#[cfg(not(feature = "tinybmp"))]
pub fn open_bmp_data(rt: &mut RuntimeState, _data: &[u8], name: &str) {
    show_error(
        rt,
        "BMP Error",
        &format!(
            "File: {}\n\nBMP support not compiled in.\nRebuild with --features tinybmp to enable.",
            name
        ),
    );
}

// ── PNG viewer ───────────────────────────────────────────────

#[cfg(feature = "minipng")]
pub fn open_png_data(rt: &mut RuntimeState, data: &[u8], _name: &str) {
    // Use minipng: decode PNG header to get dimensions
    let header = match minipng::decode_png_header(data) {
        Ok(h) => h,
        Err(e) => {
            show_error(rt, "PNG Error", &format!("Bad header:\n{:?}", e));
            return;
        }
    };
    let w = header.width() as u32;
    let h = header.height() as u32;
    if w > MAX_IMG_W || h > MAX_IMG_H {
        show_error(rt, "PNG Error", &format!("Image too large: {}x{}", w, h));
        return;
    }

    log_status!("PNG before decode");

    // Full decode into page-backed buffer (bypasses kernel heap)
    let buf_len = (w as usize) * (h as usize) * 4;
    let mut page_buf = match unsafe { PageBuf::<u8>::alloc_zeroed_for_len(buf_len) } {
        Some(buf) => buf,
        None => {
            show_error(rt, "PNG Error", "Out of memory for pixel buffer");
            return;
        }
    };
    let img = match minipng::decode_png(data, page_buf.as_mut_slice()) {
        Ok(img) => img,
        Err(e) => {
            show_error(rt, "PNG Error", &format!("Decode failed:\n{:?}", e));
            return;
        }
    };
    log_status!("PNG after decode (pixels on PageBuf)");

    let win_w = w.min(800).max(160);
    let win_h = h.min(600).max(120);
    let id = rt
        .desktop
        .wm
        .create_titled_window(120, 80, win_w, win_h, 0x000000, "Image Viewer");
    if let Some(win) = rt.desktop.wm.windows_mut().iter_mut().find(|w| w.id == id) {
        let pixels = img.pixels();
        for y in 0..h.min(win_h) {
            for x in 0..w.min(win_w) {
                let pi = ((y as usize) * (w as usize) + (x as usize)) * 4;
                if pi + 3 < pixels.len() {
                    let color = (pixels[pi] as u32) << 16
                        | (pixels[pi + 1] as u32) << 8
                        | (pixels[pi + 2] as u32);
                    win.surface.set_pixel(x, y, color);
                }
            }
        }
        rt.desktop.invalidate_window(id);
    }
    rt.desktop.wm.raise_to_top(id);
    rt.frame_due = true;
}

// ── JPEG viewer ──────────────────────────────────────────────

/// Decoded JPEG image, produced without holding the runtime lock.
#[cfg(feature = "zune-jpeg")]
pub struct DecodedJpeg {
    pub width: u16,
    pub height: u16,
    pub pixels: Vec<u8>,
}

#[cfg(feature = "zune-jpeg")]
fn is_jpeg_sof_marker(marker: u8) -> bool {
    matches!(
        marker,
        0xC0 | 0xC1 | 0xC2 | 0xC3 | 0xC5 | 0xC6 | 0xC7 | 0xC9 | 0xCA | 0xCB | 0xCD | 0xCE | 0xCF
    )
}

/// Inspect a JPEG prefix and return dimensions once the SOF marker is present.
#[cfg(feature = "zune-jpeg")]
pub fn preflight_jpeg_header(data: &[u8]) -> Result<Option<(u16, u16)>, String> {
    if data.len() < 2 {
        return Ok(None);
    }
    if data.get(..2) != Some(&[0xFF, 0xD8]) {
        return Err(String::from("Not a JPEG file"));
    }

    let mut pos = 2;
    while pos < data.len() {
        while pos < data.len() && data[pos] != 0xFF {
            pos += 1;
        }
        if pos + 1 >= data.len() {
            return Ok(None);
        }
        while pos < data.len() && data[pos] == 0xFF {
            pos += 1;
        }
        if pos >= data.len() {
            return Ok(None);
        }
        let marker = data[pos];
        pos += 1;

        if matches!(marker, 0x01 | 0xD0..=0xD9) {
            if marker == 0xD9 {
                return Err(String::from("JPEG ended before image dimensions"));
            }
            continue;
        }
        if pos + 2 > data.len() {
            return Ok(None);
        }
        let segment_len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
        if segment_len < 2 {
            return Err(String::from("Invalid JPEG segment length"));
        }
        let segment_start = pos + 2;
        let segment_end = pos + segment_len;
        if segment_end > data.len() {
            return Ok(None);
        }

        if is_jpeg_sof_marker(marker) {
            if segment_len < 7 {
                return Err(String::from("Invalid JPEG SOF segment"));
            }
            let height = u16::from_be_bytes([data[segment_start + 1], data[segment_start + 2]]);
            let width = u16::from_be_bytes([data[segment_start + 3], data[segment_start + 4]]);
            if u32::from(width) > MAX_IMG_W || u32::from(height) > MAX_IMG_H {
                return Err(format!("Image too large: {}x{}", width, height));
            }
            return Ok(Some((width, height)));
        }

        if marker == 0xDA {
            return Err(String::from("JPEG scan started before image dimensions"));
        }
        pos = segment_end;
    }

    Ok(None)
}

/// Decode JPEG data without any runtime lock held.
/// Call this before acquiring the runtime lock to avoid UI freezes.
#[cfg(feature = "zune-jpeg")]
pub fn decode_jpeg(data: &[u8]) -> Result<DecodedJpeg, String> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;

    log_status!("JPEG file read ({} B)", data.len());

    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .set_max_width(MAX_IMG_W as usize)
        .set_max_height(MAX_IMG_H as usize);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(data), options);

    // Step 1: Parse headers only (no pixel buffer allocation yet).
    decoder
        .decode_headers()
        .map_err(|e| format!("JPEG header error: {:?}", e))?;

    // Step 2: Get image dimensions and calculate pixel buffer size.
    let info = decoder
        .info()
        .ok_or_else(|| String::from("Missing JPEG image information"))?;
    let width = u32::from(info.width);
    let height = u32::from(info.height);
    if width > MAX_IMG_W || height > MAX_IMG_H {
        return Err(format!("Image too large: {}x{}", width, height));
    }
    let buffer_size = decoder
        .output_buffer_size()
        .ok_or_else(|| String::from("JPEG output buffer size overflow"))?;
    // Add extra overhead for decoder's internal working buffers (rough estimate).
    let total_needed = buffer_size.saturating_add(256 * 1024);

    log_status!("JPEG {}x{} buffer={}B", width, height, total_needed);

    // Step 3: Ensure heap has enough room for the decoded pixels.
    let heap_free = petroleum::heap_stats().free;
    if heap_free < total_needed {
        let additional = total_needed
            .saturating_sub(heap_free)
            .next_multiple_of(4096);
        let extend_fn = RUNTIME_CONTEXT.callback_snapshot().heap_extend;
        match extend_fn {
            Some(f) if f(additional).is_ok() => {
                HEAP_EXTEND_RESERVE.fetch_add(additional, core::sync::atomic::Ordering::Relaxed);
            }
            _ => {
                return Err(format!(
                    "Cannot allocate {} bytes for JPEG decode (heap free: {})",
                    total_needed, heap_free
                ));
            }
        }
    }

    // Step 4: Pre-flight sanity check; reject images whose pixel buffer would
    // require an unreasonable number of MCU rows (more than 4× the height limit)
    // as a heuristic guard against decoder hangs on invalid scan data.
    if buffer_size > (MAX_IMG_W * MAX_IMG_H * 4) as usize {
        return Err(format!(
            "JPEG buffer size {} exceeds maximum ({}x{}x4)",
            buffer_size, MAX_IMG_W, MAX_IMG_H,
        ));
    }

    // Step 5: Decode pixel data (heap should now have space).
    log_status!("JPEG calling decode()");
    let pixels = decoder
        .decode()
        .map_err(|e| format!("JPEG decode error: {:?}", e))?;
    log_status!(
        "JPEG decode done ({}x{} pixels={}B)",
        info.width,
        info.height,
        pixels.len()
    );
    Ok(DecodedJpeg {
        width: info.width,
        height: info.height,
        pixels,
    })
}

/// Render a decoded JPEG into a new window.
/// Must be called with the runtime lock held.
#[cfg(feature = "zune-jpeg")]
pub fn render_jpeg_window(rt: &mut RuntimeState, decoded: DecodedJpeg, _name: &str) {
    let width = u32::from(decoded.width);
    let height = u32::from(decoded.height);

    let win_w = width.min(800).max(160);
    let win_h = height.min(600).max(120);
    let id = rt
        .desktop
        .wm
        .create_titled_window(120, 80, win_w, win_h, 0x000000, "Image Viewer");
    if let Some(window) = rt.desktop.wm.windows_mut().iter_mut().find(|w| w.id == id) {
        let px = decoded.pixels.as_slice();
        for y in 0..height.min(win_h) {
            for x in 0..width.min(win_w) {
                let offset = ((y as usize) * (width as usize) + x as usize) * 3;
                if offset + 2 < px.len() {
                    let color = (px[offset] as u32) << 16
                        | (px[offset + 1] as u32) << 8
                        | px[offset + 2] as u32;
                    window.surface.set_pixel(x, y, color);
                }
            }
        }
        rt.desktop.invalidate_window(id);
    }
    rt.desktop.wm.raise_to_top(id);
    rt.frame_due = true;
}

// ── WAV info viewer ─────────────────────────────────────────

pub fn open_wav_data(rt: &mut RuntimeState, data: &[u8], name: &str) {
    // Manual WAV parsing (pure_wav crate API is streaming-oriented)
    if data.len() < 44 || &data[..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        show_error(rt, "WAV Error", "Not a valid WAV file");
        return;
    }
    let channels = u16::from_le_bytes([data[22], data[23]]);
    let sample_rate = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
    let bits_per_sample = u16::from_le_bytes([data[34], data[35]]);

    // Find data chunk size
    let mut data_size = 0u32;
    let mut off = 36;
    while off + 8 <= data.len() {
        let chunk_len =
            u32::from_le_bytes([data[off + 4], data[off + 5], data[off + 6], data[off + 7]]);
        if &data[off..off + 4] == b"data" {
            data_size = chunk_len;
            break;
        }
        off += 8 + chunk_len as usize;
    }

    let duration = if sample_rate > 0 && bits_per_sample > 0 {
        (data_size as f64) / (channels as f64 * (bits_per_sample as f64 / 8.0) * sample_rate as f64)
    } else {
        0.0
    };

    let msg = format!(
        "File: {}\n\nFormat: WAV\nChannels: {}\nSample Rate: {} Hz\nBits: {}-bit\nData size: {} bytes\nDuration: {:.1} s\n\nPlayback not yet implemented.",
        name, channels, sample_rate, bits_per_sample, data_size, duration,
    );
    show_text_window(rt, "Music Player", &msg, 50, 0x0d0d1a, 0xCCFFCC);
}

// ── MP3 info viewer ─────────────────────────────────────────

#[derive(Debug, PartialEq)]
struct Mp3Info {
    frames: u32,
    sample_rate: u32,
    channels: u16,
    duration_seconds: f64,
}

#[derive(Clone, Copy)]
struct Mp3FrameHeader {
    frame_len: usize,
    sample_rate: u32,
    channels: u16,
    samples: u32,
}

fn mp3_audio_start(data: &[u8]) -> usize {
    if data.len() < 10 || &data[..3] != b"ID3" {
        return 0;
    }

    let size_bytes = &data[6..10];
    if size_bytes.iter().any(|byte| byte & 0x80 != 0) {
        return 0;
    }
    let tag_size = size_bytes
        .iter()
        .fold(0usize, |size, byte| (size << 7) | usize::from(*byte));
    let footer_size = if data[5] & 0x10 != 0 { 10 } else { 0 };
    10usize
        .checked_add(tag_size)
        .and_then(|size| size.checked_add(footer_size))
        .unwrap_or(data.len())
        .min(data.len())
}

fn parse_mp3_frame_header(data: &[u8], offset: usize) -> Option<Mp3FrameHeader> {
    let bytes = data.get(offset..offset.checked_add(4)?)?;
    let header = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if header & 0xffe0_0000 != 0xffe0_0000 {
        return None;
    }

    let version = (header >> 19) & 0x3;
    let layer = (header >> 17) & 0x3;
    let bitrate_index = ((header >> 12) & 0xf) as usize;
    let sample_rate_index = ((header >> 10) & 0x3) as usize;
    if version == 1
        || layer != 1
        || bitrate_index == 0
        || bitrate_index == 15
        || sample_rate_index == 3
    {
        return None;
    }

    const MPEG1_BITRATES: [u32; 16] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
    ];
    const MPEG2_BITRATES: [u32; 16] = [
        0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
    ];
    const MPEG1_SAMPLE_RATES: [u32; 3] = [44_100, 48_000, 32_000];

    let bitrate_kbps = if version == 3 {
        MPEG1_BITRATES[bitrate_index]
    } else {
        MPEG2_BITRATES[bitrate_index]
    };
    let sample_rate = match version {
        3 => MPEG1_SAMPLE_RATES[sample_rate_index],
        2 => MPEG1_SAMPLE_RATES[sample_rate_index] / 2,
        0 => MPEG1_SAMPLE_RATES[sample_rate_index] / 4,
        _ => return None,
    };
    let padding = ((header >> 9) & 1) as u32;
    let (coefficient, samples) = if version == 3 {
        (144_000u32, 1_152u32)
    } else {
        (72_000u32, 576u32)
    };
    let frame_len = coefficient
        .checked_mul(bitrate_kbps)?
        .checked_div(sample_rate)?
        .checked_add(padding)?;
    let channels = if (header >> 6) & 0x3 == 3 { 1 } else { 2 };

    Some(Mp3FrameHeader {
        frame_len: usize::try_from(frame_len).ok()?,
        sample_rate,
        channels,
        samples,
    })
}

fn parse_mp3(data: &[u8]) -> Option<Mp3Info> {
    let mut offset = mp3_audio_start(data);
    let mut frames = 0u32;
    let mut sample_rate = 0u32;
    let mut channels = 0u16;
    let mut duration_seconds = 0.0;

    while offset.checked_add(4)? <= data.len() {
        let Some(header) = parse_mp3_frame_header(data, offset) else {
            offset = offset.checked_add(1)?;
            continue;
        };
        let frame_end = offset.checked_add(header.frame_len)?;
        if frame_end > data.len() {
            offset = offset.checked_add(1)?;
            continue;
        }

        if frames == 0 {
            sample_rate = header.sample_rate;
            channels = header.channels;
        }
        frames = frames.saturating_add(1);
        duration_seconds += f64::from(header.samples) / f64::from(header.sample_rate);
        offset = frame_end;
    }

    (frames > 0).then_some(Mp3Info {
        frames,
        sample_rate,
        channels,
        duration_seconds,
    })
}

pub fn open_mp3_data(rt: &mut RuntimeState, data: &[u8], name: &str) {
    let Some(info) = parse_mp3(data) else {
        show_error(rt, "MP3 Error", "No valid MP3 audio frames found.");
        return;
    };
    let msg = format!(
        "File: {}\n\nFormat: MP3\nChannels: {}\nSample Rate: {} Hz\nFrames: {}\nDuration: {:.1} s\n\nPlayback not yet implemented.",
        name, info.channels, info.sample_rate, info.frames, info.duration_seconds,
    );
    show_text_window(rt, "Music Player", &msg, 50, 0x0d0d1a, 0xCCCCFF);
}

// ── Tar archive listing ─────────────────────────────────────

fn parse_tar_octal(field: &[u8]) -> u64 {
    let mut value = 0u64;
    let mut saw_digit = false;
    for byte in field {
        match *byte {
            b'0'..=b'7' => {
                saw_digit = true;
                value = value
                    .checked_mul(8)
                    .and_then(|value| value.checked_add(u64::from(*byte - b'0')))
                    .unwrap_or(u64::MAX);
            }
            0 | b' ' if !saw_digit => continue,
            0 | b' ' => break,
            _ => break,
        }
    }
    value
}

fn tar_entries(data: &[u8]) -> Vec<String> {
    let mut entries = Vec::new();
    let mut off = 0usize;
    while off + 512 <= data.len() {
        let block = &data[off..off + 512];
        if block[0] == 0 {
            break;
        }

        let name_end = block[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let entry_name = core::str::from_utf8(&block[..name_end]).unwrap_or("(invalid)");
        let size = parse_tar_octal(&block[124..136]);
        let type_flag = block[156];
        let kind = match type_flag {
            b'5' => "dir",
            b'2' => "link",
            _ => "file",
        };
        entries.push(format!("{} {:>8}  {}", kind, size, entry_name));
        let Some(blocks) = size.checked_add(511).map(|s| s / 512 * 512) else {
            break;
        };
        let Some(total) = (512 as usize).checked_add(blocks as usize) else {
            break;
        };
        off = match off.checked_add(total) {
            Some(o) => o,
            None => break,
        };
    }
    entries
}

fn show_archive_entries(rt: &mut RuntimeState, name: &str, entries: &[String]) {
    let mut msg = format!("Archive: {}\n{} entries\n\n", name, entries.len());
    for e in entries {
        if msg.len() < 2000 {
            msg.push_str(e);
            msg.push('\n');
        }
    }
    if entries.is_empty() {
        msg.push_str("(empty archive)\n");
    }
    show_text_window(rt, "Archive Manager", &msg, 60, 0x0d1a0d, 0xCCFFCC);
}

pub fn open_tar_data(rt: &mut RuntimeState, data: &[u8], name: &str) {
    show_archive_entries(rt, name, &tar_entries(data));
}

// ── gzip / tgz archive support ───────────────────────────────

#[cfg(feature = "gzip")]
fn decompress_gzip(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    if data.len() < 18 || data[0..2] != [0x1f, 0x8b] || data[2] != 8 {
        return Err("invalid gzip header");
    }
    let flags = data[3];
    if flags & 0xe0 != 0 {
        return Err("unsupported gzip flags");
    }
    let mut offset = 10usize;
    if flags & 0x04 != 0 {
        let length = data
            .get(offset..offset + 2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]) as usize)
            .ok_or("truncated gzip extra field")?;
        offset = offset
            .checked_add(2 + length)
            .ok_or("gzip header overflow")?;
    }
    for flag in [0x08, 0x10] {
        if flags & flag != 0 {
            let tail = data.get(offset..).ok_or("truncated gzip string")?;
            let length = tail
                .iter()
                .position(|byte| *byte == 0)
                .ok_or("unterminated gzip string")?;
            offset = offset
                .checked_add(length + 1)
                .ok_or("gzip header overflow")?;
        }
    }
    if flags & 0x02 != 0 {
        offset = offset.checked_add(2).ok_or("gzip header overflow")?;
    }
    let compressed = data
        .get(offset..data.len().saturating_sub(8))
        .ok_or("truncated gzip payload")?;
    miniz_oxide::inflate::decompress_to_vec_with_limit(compressed, 32 * 1024 * 1024)
        .map_err(|_| "gzip decompression failed")
}

#[cfg(feature = "gzip")]
pub fn open_gzip_data(rt: &mut RuntimeState, data: &[u8], name: &str, tar: bool) {
    let decoded = match decompress_gzip(data) {
        Ok(decoded) => decoded,
        Err(error) => {
            show_error(rt, "gzip Error", error);
            return;
        }
    };
    if tar {
        show_archive_entries(rt, name, &tar_entries(&decoded));
    } else {
        let preview = core::str::from_utf8(&decoded)
            .map(|text| text.chars().take(1600).collect::<alloc::string::String>())
            .unwrap_or_else(|_| alloc::string::String::from("(binary data)"));
        let message = format!(
            "File: {}\nUncompressed size: {} bytes\n\n{}",
            name,
            decoded.len(),
            preview
        );
        show_text_window(rt, "gzip Viewer", &message, 60, 0x0d1a0d, 0xCCFFCC);
    }
}

// ── ZIP central directory listing ────────────────────────────

fn zip_entries(data: &[u8]) -> Vec<String> {
    const CENTRAL_HEADER: &[u8; 4] = b"PK\x01\x02";
    let mut entries = Vec::new();
    let mut offset = 0usize;
    while offset + 46 <= data.len() {
        let Some(relative) = data[offset..]
            .windows(4)
            .position(|window| window == CENTRAL_HEADER)
        else {
            break;
        };
        offset += relative;
        let header = &data[offset..offset + 46];
        let compressed = u32::from_le_bytes(header[20..24].try_into().unwrap());
        let uncompressed = u32::from_le_bytes(header[24..28].try_into().unwrap());
        let name_len = u16::from_le_bytes(header[28..30].try_into().unwrap()) as usize;
        let extra_len = u16::from_le_bytes(header[30..32].try_into().unwrap()) as usize;
        let comment_len = u16::from_le_bytes(header[32..34].try_into().unwrap()) as usize;
        let end = match offset
            .checked_add(46)
            .and_then(|value| value.checked_add(name_len + extra_len + comment_len))
        {
            Some(end) if end <= data.len() => end,
            _ => break,
        };
        let name =
            core::str::from_utf8(&data[offset + 46..offset + 46 + name_len]).unwrap_or("(invalid)");
        entries.push(format!(
            "{:>8} -> {:>8}  {}",
            uncompressed, compressed, name
        ));
        offset = end;
    }
    entries
}

pub fn open_zip_data(rt: &mut RuntimeState, data: &[u8], name: &str) {
    show_archive_entries(rt, name, &zip_entries(data));
}

// ── MP4 info viewer ──────────────────────────────────────────

#[cfg(feature = "shiguredo_mp4")]
struct Mp4Summary {
    major_brand: String,
    compatible_brands: Vec<String>,
    has_moov: bool,
    duration_seconds: Option<u64>,
}

#[cfg(feature = "shiguredo_mp4")]
fn mp4_brand(bytes: &[u8]) -> String {
    use alloc::string::ToString;

    core::str::from_utf8(bytes)
        .map(|brand| brand.trim_matches(char::from(0)).to_string())
        .unwrap_or_else(|_| String::from("????"))
}

#[cfg(feature = "shiguredo_mp4")]
fn mp4_box_size(data: &[u8], offset: usize) -> Option<(usize, [u8; 4], usize)> {
    if offset.checked_add(8)? > data.len() {
        return None;
    }
    let size32 = u32::from_be_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
    let kind: [u8; 4] = data[offset + 4..offset + 8].try_into().ok()?;
    match size32 {
        0 => Some((data.len().saturating_sub(offset), kind, 8)),
        1 => {
            if offset.checked_add(16)? > data.len() {
                return None;
            }
            let size64 = u64::from_be_bytes(data[offset + 8..offset + 16].try_into().ok()?);
            let size = usize::try_from(size64).ok()?;
            Some((size, kind, 16))
        }
        size => Some((size, kind, 8)),
    }
}

#[cfg(feature = "shiguredo_mp4")]
fn parse_mp4_summary(data: &[u8]) -> Mp4Summary {
    let mut summary = Mp4Summary {
        major_brand: String::from("unknown"),
        compatible_brands: Vec::new(),
        has_moov: false,
        duration_seconds: None,
    };

    let mut offset = 0usize;
    let mut boxes_seen = 0usize;
    while offset + 8 <= data.len() && boxes_seen < 128 {
        boxes_seen += 1;
        let Some((size, kind, header)) = mp4_box_size(data, offset) else {
            break;
        };
        if size < header {
            break;
        }
        let end = match offset.checked_add(size) {
            Some(end) if end <= data.len() => end,
            _ => break,
        };
        let payload = &data[offset + header..end];
        match &kind {
            b"ftyp" if payload.len() >= 8 => {
                summary.major_brand = mp4_brand(&payload[..4]);
                summary.compatible_brands.clear();
                for brand in payload[8..].chunks_exact(4).take(8) {
                    summary.compatible_brands.push(mp4_brand(brand));
                }
            }
            b"moov" => {
                summary.has_moov = true;
                parse_moov_summary(payload, &mut summary);
            }
            _ => {}
        }
        offset = end;
    }

    summary
}

#[cfg(feature = "shiguredo_mp4")]
fn parse_moov_summary(data: &[u8], summary: &mut Mp4Summary) {
    let mut offset = 0usize;
    let mut boxes_seen = 0usize;
    while offset + 8 <= data.len() && boxes_seen < 128 {
        boxes_seen += 1;
        let Some((size, kind, header)) = mp4_box_size(data, offset) else {
            break;
        };
        if size < header {
            break;
        }
        let end = match offset.checked_add(size) {
            Some(end) if end <= data.len() => end,
            _ => break,
        };
        let payload = &data[offset + header..end];
        if &kind == b"mvhd" {
            summary.duration_seconds = parse_mvhd_duration(payload);
        }
        offset = end;
    }
}

#[cfg(feature = "shiguredo_mp4")]
fn parse_mvhd_duration(payload: &[u8]) -> Option<u64> {
    let version = *payload.first()?;
    match version {
        0 if payload.len() >= 20 => {
            let timescale = u32::from_be_bytes(payload[12..16].try_into().ok()?);
            let duration = u32::from_be_bytes(payload[16..20].try_into().ok()?);
            (timescale != 0).then_some(u64::from(duration / timescale))
        }
        1 if payload.len() >= 32 => {
            let timescale = u32::from_be_bytes(payload[20..24].try_into().ok()?);
            let duration = u64::from_be_bytes(payload[24..32].try_into().ok()?);
            (timescale != 0).then_some(duration / u64::from(timescale))
        }
        _ => None,
    }
}

#[cfg(feature = "shiguredo_mp4")]
pub fn open_mp4_data(rt: &mut RuntimeState, data: &[u8], name: &str) {
    let summary = parse_mp4_summary(data);
    let mut brands = String::new();
    for (index, brand) in summary.compatible_brands.iter().enumerate() {
        if index > 0 {
            brands.push_str(", ");
        }
        brands.push_str(brand);
    }
    if brands.is_empty() {
        brands.push_str("(none found in prefix)");
    }
    let duration = summary
        .duration_seconds
        .map(|seconds| format!("{} s", seconds))
        .unwrap_or_else(|| String::from("(not found in prefix)"));
    let msg = format!(
        "File: {}\nFormat: MP4\nMajor brand: {}\nCompatible: {}\nMovie box: {}\nDuration: {}\n\nPlayback not yet implemented.",
        name,
        summary.major_brand,
        brands,
        if summary.has_moov {
            "found"
        } else {
            "not in prefix"
        },
        duration,
    );
    show_text_window(rt, "Movie Player", &msg, 50, 0x0d0d1a, 0xCCCCFF);
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "shiguredo_mp4")]
    use super::parse_mp4_summary;
    #[cfg(feature = "zune-jpeg")]
    use super::preflight_jpeg_header;
    use super::{parse_mp3, tar_entries, zip_entries};
    use alloc::vec;
    use alloc::vec::Vec;

    #[test]
    fn tar_listing_parses_a_regular_file() {
        let mut archive = vec![0u8; 1024];
        archive[..9].copy_from_slice(b"hello.txt");
        archive[124..135].copy_from_slice(b"00000000005");
        archive[156] = b'0';
        archive[512..517].copy_from_slice(b"hello");

        let entries = tar_entries(&archive);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].contains("hello.txt"));
        assert!(entries[0].contains('5'));
    }

    #[test]
    fn zip_listing_parses_central_directory_metadata() {
        let name = b"notes.md";
        let mut archive = vec![0u8; 46 + name.len()];
        archive[..4].copy_from_slice(b"PK\x01\x02");
        archive[20..24].copy_from_slice(&4u32.to_le_bytes());
        archive[24..28].copy_from_slice(&8u32.to_le_bytes());
        archive[28..30].copy_from_slice(&(name.len() as u16).to_le_bytes());
        archive[46..].copy_from_slice(name);

        let entries = zip_entries(&archive);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].contains("notes.md"));
    }

    #[test]
    fn mp3_parser_skips_id3_and_counts_layer_three_frames() {
        const FRAME_LEN: usize = 417;
        let mut data = vec![0u8; 10 + FRAME_LEN * 2];
        data[..3].copy_from_slice(b"ID3");
        data[3] = 4;
        data[10..14].copy_from_slice(&[0xff, 0xfb, 0x90, 0x64]);
        data[10 + FRAME_LEN..14 + FRAME_LEN].copy_from_slice(&[0xff, 0xfb, 0x90, 0x64]);

        let info = parse_mp3(&data).expect("valid MP3 frames");
        assert_eq!(info.frames, 2);
        assert_eq!(info.sample_rate, 44_100);
        assert_eq!(info.channels, 2);
        assert!((info.duration_seconds - 2_304.0 / 44_100.0).abs() < 0.000_001);
    }

    #[cfg(feature = "shiguredo_mp4")]
    #[test]
    fn mp4_summary_reads_brand_and_movie_duration_from_prefix() {
        let mut data = Vec::new();
        data.extend_from_slice(&24u32.to_be_bytes());
        data.extend_from_slice(b"ftyp");
        data.extend_from_slice(b"isom");
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(b"isom");
        data.extend_from_slice(b"mp42");

        let mut mvhd_payload = vec![0u8; 20];
        mvhd_payload[12..16].copy_from_slice(&1000u32.to_be_bytes());
        mvhd_payload[16..20].copy_from_slice(&12_000u32.to_be_bytes());
        let mvhd_size = (8 + mvhd_payload.len()) as u32;
        let moov_size = 8 + mvhd_size;
        data.extend_from_slice(&(moov_size as u32).to_be_bytes());
        data.extend_from_slice(b"moov");
        data.extend_from_slice(&mvhd_size.to_be_bytes());
        data.extend_from_slice(b"mvhd");
        data.extend_from_slice(&mvhd_payload);

        let summary = parse_mp4_summary(&data);
        assert_eq!(summary.major_brand, "isom");
        assert!(summary.has_moov);
        assert_eq!(summary.duration_seconds, Some(12));
        assert!(
            summary
                .compatible_brands
                .iter()
                .any(|brand| brand == "mp42")
        );
    }

    #[cfg(feature = "zune-jpeg")]
    #[test]
    fn jpeg_preflight_reads_dimensions_from_prefix() {
        let jpeg = [
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x12, 0x34, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00,
            0x01, 0x00, 0x02, 0x01, 0x01, 0x11, 0x00,
        ];

        assert_eq!(preflight_jpeg_header(&jpeg).unwrap(), Some((2, 1)));

        let mut oversized = jpeg;
        oversized[13..15].copy_from_slice(&1081u16.to_be_bytes());
        assert!(
            preflight_jpeg_header(&oversized)
                .unwrap_err()
                .contains("Image too large")
        );

        assert_eq!(preflight_jpeg_header(&jpeg[..8]).unwrap(), None);
    }

    #[cfg(feature = "zune-jpeg")]
    #[test]
    fn jpeg_decode_headers_and_buffer_size() {
        // Minimal 1x1 grey JPEG (contains SOI, APP0/JFIF, DQT, SOF0, DHT, SOS, EOI).
        let jpeg: &[u8] = &[
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00,
            0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0xff, 0xdb, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06,
            0x07, 0x06, 0x05, 0x08, 0x07, 0x07, 0x07, 0x09, 0x09, 0x08, 0x0a, 0x0c, 0x14, 0x0d,
            0x0c, 0x0b, 0x0b, 0x0c, 0x19, 0x12, 0x13, 0x0f, 0x14, 0x1d, 0x1a, 0x1f, 0x1e, 0x1d,
            0x1a, 0x1c, 0x1c, 0x20, 0x24, 0x2e, 0x27, 0x20, 0x22, 0x2c, 0x23, 0x1c, 0x1c, 0x28,
            0x37, 0x29, 0x2c, 0x30, 0x31, 0x34, 0x34, 0x34, 0x1f, 0x27, 0x39, 0x3d, 0x38, 0x32,
            0x3c, 0x2e, 0x33, 0x34, 0x32, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x00, 0x01, 0x00, 0x01,
            0x01, 0x01, 0x11, 0x00, 0xff, 0xc4, 0x00, 0x1f, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01,
            0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02,
            0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0xff, 0xc4, 0x00, 0xb5, 0x10,
            0x00, 0x02, 0x01, 0x03, 0x03, 0x02, 0x04, 0x03, 0x05, 0x05, 0x04, 0x04, 0x00, 0x00,
            0x01, 0x7d, 0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06,
            0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42,
            0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16,
            0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37,
            0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55,
            0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73,
            0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
            0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5,
            0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba,
            0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6,
            0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea,
            0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xff, 0xda, 0x00, 0x08,
            0x01, 0x01, 0x00, 0x00, 0x3f, 0x00, 0x7b, 0x40, 0x00, 0xff, 0xd9,
        ];

        // Verify the direct zune_jpeg decoder works (independent of kernel heap).
        use zune_core::bytestream::ZCursor;
        use zune_core::colorspace::ColorSpace;
        use zune_core::options::DecoderOptions;

        let options = DecoderOptions::default()
            .jpeg_set_out_colorspace(ColorSpace::RGB)
            .set_max_width(1920)
            .set_max_height(1080);
        let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(jpeg), options);

        // decode_headers should parse the header without allocating pixel data.
        decoder
            .decode_headers()
            .expect("JPEG header decode should succeed");

        // info() should be available after decode_headers().
        let info = decoder
            .info()
            .expect("JPEG info should be available after header decode");
        assert_eq!(info.width, 1, "JPEG width should be 1");
        assert_eq!(info.height, 1, "JPEG height should be 1");

        // output_buffer_size() should return the expected pixel buffer size.
        let buf_size = decoder
            .output_buffer_size()
            .expect("output_buffer_size should return some value");
        assert_eq!(buf_size, 3, "1x1 RGB pixel buffer should be 3 bytes");

        // decode should succeed and produce the expected pixel data.
        let pixels = decoder.decode().expect("JPEG pixel decode should succeed");
        assert_eq!(pixels.len(), 3, "Decoded pixel buffer should be 3 bytes");
        // 1x1 grey JPEG decodes to a single RGB pixel (values may be near-zero for dark grey).
        assert_eq!(pixels.len(), 3, "All 3 pixel bytes must be present");
    }
}
