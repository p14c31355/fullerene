//! Media file viewers — BMP, PNG, WAV, MP3, tar content display.
//!
//! Each viewer reads file data via `vfs_read`, parses the format, and
//! creates a window with the content rendered into the surface.

use crate::{GLYPH_H, RUNTIME_CONTEXT, RuntimeState};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

const MAX_IMG_W: u32 = 1920;
const MAX_IMG_H: u32 = 1080;
const GLYPH_SIZE: u32 = 8;

fn read_file(path: &str) -> Result<Vec<u8>, genome::FsError> {
    let read_fn = RUNTIME_CONTEXT
        .callback_snapshot()
        .vfs_read
        .ok_or(genome::FsError::NotSupported)?;
    read_fn(path)
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

fn show_error(rt: &mut RuntimeState, title: &str, msg: &str) {
    show_text_window(rt, title, msg, 50, 0x1a1a0d, 0xFFCCCC);
}

// ── BMP viewer (tinybmp) ─────────────────────────────────────

#[cfg(feature = "tinybmp")]
pub fn open_bmp(rt: &mut RuntimeState, path: &str, _name: &str) {
    let data = match read_file(path) {
        Ok(d) => d,
        Err(e) => {
            show_error(rt, "BMP Error", &format!("Cannot read: {}", e));
            return;
        }
    };
    let bmp = match tinybmp::RawBmp::from_slice(&data) {
        Ok(b) => b,
        Err(_) => {
            show_error(rt, "BMP Error", "Parse failed");
            return;
        }
    };
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
pub fn open_bmp(rt: &mut RuntimeState, _path: &str, name: &str) {
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
pub fn open_png(rt: &mut RuntimeState, path: &str, _name: &str) {
    let data = match read_file(path) {
        Ok(d) => d,
        Err(e) => {
            show_error(rt, "PNG Error", &format!("Cannot read:\n{}", e));
            return;
        }
    };

    // Use minipng: decode PNG header to get dimensions
    let header = match minipng::decode_png_header(&data) {
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

    // Full decode
    let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
    let img = match minipng::decode_png(&data, &mut buf) {
        Ok(img) => img,
        Err(e) => {
            show_error(rt, "PNG Error", &format!("Decode failed:\n{:?}", e));
            return;
        }
    };

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

#[cfg(feature = "zune-jpeg")]
pub fn open_jpeg(rt: &mut RuntimeState, path: &str, _name: &str) {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;

    let data = match read_file(path) {
        Ok(data) => data,
        Err(error) => {
            show_error(rt, "JPEG Error", &format!("Cannot read:\n{}", error));
            return;
        }
    };
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .set_max_width(MAX_IMG_W as usize)
        .set_max_height(MAX_IMG_H as usize);
    let mut decoder =
        zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(data.as_slice()), options);
    let pixels = match decoder.decode() {
        Ok(pixels) => pixels,
        Err(error) => {
            show_error(rt, "JPEG Error", &format!("Decode failed:\n{:?}", error));
            return;
        }
    };
    let Some(info) = decoder.info() else {
        show_error(rt, "JPEG Error", "Missing image information");
        return;
    };
    let width = u32::from(info.width);
    let height = u32::from(info.height);
    if width > MAX_IMG_W || height > MAX_IMG_H {
        show_error(
            rt,
            "JPEG Error",
            &format!("Image too large: {}x{}", width, height),
        );
        return;
    }
    let win_w = width.min(800).max(160);
    let win_h = height.min(600).max(120);
    let id = rt
        .desktop
        .wm
        .create_titled_window(120, 80, win_w, win_h, 0x000000, "Image Viewer");
    if let Some(window) = rt.desktop.wm.windows_mut().iter_mut().find(|w| w.id == id) {
        for y in 0..height.min(win_h) {
            for x in 0..width.min(win_w) {
                let offset = ((y as usize) * (width as usize) + x as usize) * 3;
                if offset + 2 < pixels.len() {
                    let color = (pixels[offset] as u32) << 16
                        | (pixels[offset + 1] as u32) << 8
                        | pixels[offset + 2] as u32;
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

pub fn open_wav(rt: &mut RuntimeState, path: &str, name: &str) {
    let data = match read_file(path) {
        Ok(d) => d,
        Err(e) => {
            show_error(rt, "WAV Error", &format!("Cannot read:\n{}", e));
            return;
        }
    };

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

pub fn open_mp3(rt: &mut RuntimeState, path: &str, name: &str) {
    let data = match read_file(path) {
        Ok(d) => d,
        Err(e) => {
            show_error(rt, "MP3 Error", &format!("Cannot read:\n{}", e));
            return;
        }
    };

    let Some(info) = parse_mp3(&data) else {
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

pub fn open_tar(rt: &mut RuntimeState, path: &str, name: &str) {
    let data = match read_file(path) {
        Ok(d) => d,
        Err(e) => {
            show_error(rt, "Tar Error", &format!("Cannot read:\n{}", e));
            return;
        }
    };
    show_archive_entries(rt, name, &tar_entries(&data));
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
pub fn open_gzip(rt: &mut RuntimeState, path: &str, name: &str, tar: bool) {
    let data = match read_file(path) {
        Ok(data) => data,
        Err(error) => {
            show_error(rt, "gzip Error", &format!("Cannot read:\n{}", error));
            return;
        }
    };
    let decoded = match decompress_gzip(&data) {
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

pub fn open_zip(rt: &mut RuntimeState, path: &str, name: &str) {
    let data = match read_file(path) {
        Ok(data) => data,
        Err(error) => {
            show_error(rt, "ZIP Error", &format!("Cannot read:\n{}", error));
            return;
        }
    };
    show_archive_entries(rt, name, &zip_entries(&data));
}

// ── MP4 player ───────────────────────────────────────────────

#[cfg(feature = "shiguredo_mp4")]
use shiguredo_mp4::TrackKind;

#[cfg(feature = "shiguredo_mp4")]
fn codec_name(entry: &shiguredo_mp4::boxes::SampleEntry) -> &'static str {
    use shiguredo_mp4::boxes::SampleEntry;
    match entry {
        SampleEntry::Avc1(_) => "H.264",
        SampleEntry::Hev1(_) | SampleEntry::Hvc1(_) => "H.265",
        SampleEntry::Vp08(_) => "VP8",
        SampleEntry::Vp09(_) => "VP9",
        SampleEntry::Av01(_) => "AV1",
        _ => "Unknown",
    }
}

#[cfg(feature = "shiguredo_mp4")]
pub fn open_mp4(rt: &mut RuntimeState, path: &str, name: &str) {
    let data = match read_file(path) {
        Ok(d) => d,
        Err(e) => {
            show_error(rt, "MP4 Error", &format!("Cannot read:\n{}", e));
            return;
        }
    };

    // Demux MP4
    let mut demuxer = shiguredo_mp4::demux::Mp4FileDemuxer::new();
    let input = shiguredo_mp4::demux::Input {
        position: 0,
        data: &data,
    };
    demuxer.handle_input(input);

    // `demuxer.tracks()` borrows demuxer, so copy the summary before sample iteration.
    let tracks_with_kind: Vec<(u32, shiguredo_mp4::TrackKind, u64, u32)> = {
        let t = match demuxer.tracks() {
            Ok(t) => t,
            Err(e) => {
                show_error(rt, "MP4 Error", &format!("No tracks: {:?}", e));
                return;
            }
        };
        t.iter()
            .map(|tr| (tr.track_id, tr.kind, tr.duration, tr.timescale.get()))
            .collect()
    };

    let mut video_track_id = None;
    let mut video_width = 0u16;
    let mut video_height = 0u16;
    let mut video_codec = "Unknown";
    let mut audio_info = Vec::new();
    let mut total_duration_ms = 0f64;

    for &(tid, kind, dur, ts) in &tracks_with_kind {
        let dur_sec = dur as f64 / ts as f64;
        total_duration_ms = total_duration_ms.max(dur_sec * 1000.0);
        match kind {
            TrackKind::Video => {
                video_track_id = Some(tid);
            }
            TrackKind::Audio => {
                audio_info.push(format!("  Audio track {}: {} s", tid, dur_sec as u32));
            }
        }
    }

    let video_track_id = match video_track_id {
        Some(id) => id,
        None => {
            show_text_window(
                rt,
                "Movie Player",
                &format!(
                    "File: {}\nFormat: MP4\n{} audio track(s)\nDuration: {:.0} s\n\nNo video track found.",
                    name,
                    audio_info.len(),
                    total_duration_ms / 1000.0,
                ),
                50,
                0x0d0d1a,
                0xCCCCFF,
            );
            return;
        }
    };

    // Sample entry metadata carries the encoded dimensions and codec.
    // Limit iterations to avoid hanging on files with unrecognized codecs.
    let mut remaining = 200u32;
    loop {
        match demuxer.next_sample() {
            Ok(Some(sample)) if sample.track.track_id == video_track_id => {
                if remaining == 0 {
                    break;
                }
                remaining -= 1;
                if let Some(entry) = sample.sample_entry {
                    if let Some((w, h)) = entry.video_resolution() {
                        video_width = w;
                        video_height = h;
                    }
                    if video_codec == "Unknown" {
                        video_codec = codec_name(entry);
                    }
                }
                if video_width > 0 || video_height > 0 || video_codec != "Unknown" {
                    break;
                }
            }
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => break,
        }
    }

    // Fallback: show info
    let msg = format!(
        "File: {}\nFormat: MP4\nVideo: {}x{} {}\n{} audio\nDuration: {:.0} s\n\nPlayback not yet implemented.",
        name,
        video_width,
        video_height,
        video_codec,
        if audio_info.is_empty() {
            "No audio".into()
        } else {
            format!("{} track(s)", audio_info.len())
        },
        total_duration_ms / 1000.0,
    );
    show_text_window(rt, "Movie Player", &msg, 50, 0x0d0d1a, 0xCCCCFF);
}

#[cfg(test)]
mod tests {
    use super::{parse_mp3, tar_entries, zip_entries};
    use alloc::vec;

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
}
