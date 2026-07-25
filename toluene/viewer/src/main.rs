use image::GenericImageView;
use std::io::Cursor;

#[link(wasm_import_module = "fullerene")]
unsafe extern "C" {
    fn show_image(width: u32, height: u32, pixels_ptr: *const u8, pixels_len: u32) -> u32;
    fn create_window(title_ptr: *const u8, title_len: u32, width: u32, height: u32) -> i32;
    fn update_window(window_id: i32, width: u32, height: u32, pixels_ptr: *const u8, pixels_len: u32) -> i32;
    fn close_window(window_id: i32) -> i32;
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: viewer <path>");
        std::process::exit(1);
    }

    let path = &args[1];
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Cannot read file: {}", e);
            std::process::exit(1);
        }
    };

    if try_image(&bytes) { return; }
    if try_mp4(path, &bytes) { return; }
    if try_mp3(&bytes) { return; }
    if try_wav(&bytes) { return; }

    // Archives
    if try_zip(&bytes) { return; }
    if try_tar(&bytes) { return; }
    if try_gzip(&bytes) { return; }

    // RLE animation (Fullerene custom format)
    if try_rle(path, &bytes) { return; }

    // Text/fallback
    if let Ok(text) = std::str::from_utf8(&bytes) {
        println!("{}", text);
        return;
    }
    print_hex(path, &bytes);
}

// ── Image ───────────────────────────────────────────────────────

fn try_image(data: &[u8]) -> bool {
    image::load_from_memory(data).ok().map(|img| {
        let (w, h) = img.dimensions();
        let pixels = img.to_rgb8().into_raw();
        let _ = unsafe { show_image(w, h, pixels.as_ptr(), pixels.len() as u32) };
    }).is_some()
}

// ── MP4 video ───────────────────────────────────────────────────

fn try_mp4(path: &str, data: &[u8]) -> bool {
    let Ok(mut reader) = mp4::Mp4Reader::read_header(Cursor::new(data), data.len() as u64) else {
        return false;
    };

    let duration = reader.duration().as_secs_f64();
    let total = duration as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    let dur = if h > 0 { format!("{}h{:02}m{:02}s", h, m, s) } else { format!("{:02}m{:02}s", m, s) };

    let size_mb = data.len() as f64 / (1024.0 * 1024.0);
    println!("File: {}", path);
    println!("Type: MP4 video");
    println!("Size: {:.1} MB", size_mb);
    println!("Duration: {}", dur);

    let video_track = reader.tracks().values().find(|t| t.track_type().ok() == Some(mp4::TrackType::Video));
    let Some(video_track) = video_track else { return true };

    let track_id = video_track.track_id();
    let width = video_track.width() as u32;
    let height = video_track.height() as u32;
    println!("Resolution: {}x{}", width, height);
    if width == 0 || height == 0 || width > 1920 || height > 1080 { return true; }
    if video_track.sample_count() == 0 { return true; }
    let Ok(Some(sample)) = reader.read_sample(track_id, 0) else { return true };

    let nals = rust_h264::nal::parse_avcc(&sample.bytes, 4);
    let mut decoder = rust_h264::decoder::Decoder::new();
    for nal in &nals {
        if let Ok(Some(frame)) = decoder.decode_nal(nal) {
            let w = frame.width as usize;
            let h = frame.height as usize;
            if w > 0 && h > 0 && w <= 1920 && h <= 1080 {
                if let Some(rgb) = yuv420_to_rgb(&frame, w, h) {
                    let title = format!("Video: {}", path);
                    unsafe {
                        let wid = create_window(title.as_ptr(), title.len() as u32, w as u32, h as u32);
                        if wid >= 0 {
                            update_window(wid, w as u32, h as u32, rgb.as_ptr(), rgb.len() as u32);
                        }
                    }
                    println!("First frame decoded ({}x{})", w, h);
                }
            }
            break;
        }
    }
    true
}

fn yuv420_to_rgb(frame: &rust_h264::decoder::Frame, width: usize, height: usize) -> Option<Vec<u8>> {
    if frame.y.len() < width * height { return None; }
    let uv_w = width / 2;
    let _uv_h = height / 2;
    let mut rgb = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        for x in 0..width {
            let yi = y * width + x;
            let ui = (y / 2) * uv_w + (x / 2);
            let vi = (y / 2) * uv_w + (x / 2);
            let yv = frame.y.get(yi).copied().unwrap_or(128) as i32;
            let uv = frame.u.get(ui).copied().unwrap_or(128) as i32 - 128;
            let vv = frame.v.get(vi).copied().unwrap_or(128) as i32 - 128;
            let r = (yv + (359 * vv) / 256).clamp(0, 255) as u8;
            let g = (yv - (88 * uv + 183 * vv) / 256).clamp(0, 255) as u8;
            let b = (yv + (454 * uv) / 256).clamp(0, 255) as u8;
            rgb.push(r); rgb.push(g); rgb.push(b);
        }
    }
    Some(rgb)
}

// ── MP3 audio ───────────────────────────────────────────────────

fn try_mp3(data: &[u8]) -> bool {
    if data.len() < 10 || (&data[..3] != b"ID3" && (data[0] & 0xff) != 0xff) { return false; }
    let id3_size = if data.len() >= 10 && &data[..3] == b"ID3" {
        ((data[6] as usize) << 21) | ((data[7] as usize) << 14)
            | ((data[8] as usize) << 7) | (data[9] as usize) + 10
    } else { 0 };

    let (mut frames, mut samples, mut sr, mut ch, mut br, mut off) = (0u64, 0u64, 0u32, 0u32, 0u32, id3_size);
    while let Some((b, s, c, spf)) = mp3_frame(data, off) {
        if frames == 0 { br = b; sr = s; ch = c; }
        frames += 1; samples += spf as u64;
        off += (144_000 * b / s).max(1) as usize;
        if frames > 10000 { break; }
    }
    let dur = if sr > 0 { samples as f64 / sr as f64 } else { 0.0 };
    println!("Type: MP3 audio\nBitrate: {} kbps\nSample rate: {} Hz\nChannels: {}", br, sr, if ch == 1 { "mono" } else { "stereo" });
    println!("Duration: {:.1} s\nFrames: {}", dur, frames);
    true
}

fn mp3_frame(data: &[u8], start: usize) -> Option<(u32, u32, u32, u32)> {
    const BR: [u32; 16] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0];
    const SR: [u32; 4] = [44100, 48000, 32000, 0];
    for off in start..data.len().saturating_sub(4) {
        let h = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
        if h & 0xffe0_0000 != 0xffe0_0000 { continue; }
        let ver = (h >> 19) & 3; let layer = (h >> 17) & 3;
        let bi = ((h >> 12) & 0xf) as usize; let si = ((h >> 10) & 3) as usize;
        if ver == 1 || layer != 1 || bi == 0 || bi == 15 || si == 3 { continue; }
        let b = BR[bi]; let s = match ver { 3 => SR[si], 2 => SR[si] / 2, _ => SR[si] / 4 };
        let c = if (h >> 6) & 3 == 3 { 1 } else { 2 };
        return Some((b, s, c, if ver == 3 { 1152 } else { 576 }));
    }
    None
}

// ── WAV audio ───────────────────────────────────────────────────

fn try_wav(data: &[u8]) -> bool {
    if data.len() < 12 || &data[..4] != b"RIFF" || &data[8..12] != b"WAVE" { return false; }
    let ch = u16::from_le_bytes([data[22], data[23]]);
    let sr = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
    let bits = u16::from_le_bytes([data[34], data[35]]);
    let (mut ds, mut off) = (0u32, 36);
    while off + 8 <= data.len() {
        let cs = u32::from_le_bytes([data[off + 4], data[off + 5], data[off + 6], data[off + 7]]);
        if &data[off..off + 4] == b"data" { ds = cs; break; }
        off += 8 + cs as usize;
    }
    let dur = if sr > 0 && ch > 0 && bits > 0 { ds as f64 / (ch as f64 * (bits as f64 / 8.0) * sr as f64) } else { 0.0 };
    println!("Type: WAV audio\nChannels: {}\nSample rate: {} Hz\nBits: {}-bit\nData: {} bytes\nDuration: {:.1} s", ch, sr, bits, ds, dur);
    true
}

// ── Archives ────────────────────────────────────────────────────

fn try_zip(data: &[u8]) -> bool {
    if data.len() < 22 || (!data.starts_with(b"PK\x03\x04") && !data.starts_with(b"PK\x05\x06")) {
        return false;
    }
    println!("Archive: ZIP");
    let mut count = 0u32;
    let mut off = 0usize;
    while off + 46 <= data.len() {
        let Some(pos) = data[off..].windows(4).position(|w| w == b"PK\x01\x02") else { break };
        off += pos;
        let name_len = u16::from_le_bytes([data[off + 28], data[off + 29]]) as usize;
        let extra_len = u16::from_le_bytes([data[off + 30], data[off + 31]]) as usize;
        let comment_len = u16::from_le_bytes([data[off + 32], data[off + 33]]) as usize;
        let end = off + 46 + name_len + extra_len + comment_len;
        if end > data.len() { break; }
        let name = std::str::from_utf8(&data[off + 46..off + 46 + name_len]).unwrap_or("(invalid)");
        if !name.ends_with('/') { count += 1; }
        println!("  {} (offset {})", name, off);
        off = end;
    }
    println!("Total entries: {}", count);
    true
}

fn try_tar(data: &[u8]) -> bool {
    // TAR detection: ustar magic at offset 257
    if data.len() < 512 || &data[257..262] != b"ustar" { return false; }
    println!("Archive: TAR");
    let mut off = 0usize;
    while off + 512 <= data.len() {
        if data[off] == 0 { break; }
        let name_end = data[off..off + 100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = std::str::from_utf8(&data[off..off + name_end]).unwrap_or("(invalid)");
        let size = data[off + 124..off + 136].iter().fold(0u64, |v, &b| v * 8 + u64::from(b - b'0'));
        let kind = match data.get(off + 156) { Some(&b'5') => "dir", Some(&b'2') => "link", _ => "file" };
        println!("  {} {:>12}  {}", kind, size, name);
        let blocks = size.saturating_add(511) / 512 * 512;
        off += 512 + blocks as usize;
    }
    true
}

fn try_gzip(data: &[u8]) -> bool {
    if data.len() < 18 || !data.starts_with(b"\x1f\x8b") || data[2] != 8 { return false; }
    println!("Archive: GZIP");
    // Try to decompress and show the first entries if it's a .tar.gz
    use std::io::Read;
    let mut decoder = flate2::read::GzDecoder::new(&data[..]);
    let mut decompressed = Vec::new();
    if decoder.read_to_end(&mut decompressed).is_ok() && decompressed.len() >= 512 && &decompressed[257..262] == b"ustar" {
        println!("  (contains TAR archive)");
        try_tar_inner(&decompressed);
    } else {
        println!("  Uncompressed size: {} bytes (preview suppressed)", decompressed.len());
    }
    true
}

fn try_tar_inner(data: &[u8]) {
    let mut off = 0usize;
    while off + 512 <= data.len() {
        if data[off] == 0 { break; }
        let name_end = data[off..off + 100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = std::str::from_utf8(&data[off..off + name_end]).unwrap_or("(invalid)");
        let size = data[off + 124..off + 136].iter().fold(0u64, |v, &b| v * 8 + u64::from(b - b'0'));
        let kind = match data.get(off + 156) { Some(&b'5') => "dir", _ => "file" };
        println!("  {} {:>12}  {}", kind, size, name);
        let blocks = size.saturating_add(511) / 512 * 512;
        off += 512 + blocks as usize;
    }
}

// ── RLE animation (Fullerene custom format) ─────────────────────

fn try_rle(path: &str, data: &[u8]) -> bool {
    if data.len() < 16 || &data[..4] != b"BARL" || u32::from_le_bytes([data[4], data[5], data[6], data[7]]) != 1 {
        return false;
    }
    let frame_count = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    let frame_width = u16::from_le_bytes([data[12], data[13]]) as u32;
    let frame_height = u16::from_le_bytes([data[14], data[15]]) as u32;
    println!("Animation: RLE");
    println!("Frames: {}", frame_count);
    println!("Resolution: {}x{}", frame_width, frame_height);

    if frame_width == 0 || frame_height == 0 || frame_width > 1920 || frame_height > 1080 || frame_count == 0 {
        return true;
    }

    // Decode and display the first frame
    let pix_count = frame_width as usize * frame_height as usize;
    let mut pixels = vec![0u8; pix_count * 3];
    let mut off = 16 + frame_count as usize * 2; // skip header + offset table

    // Parse each frame's data (simple RLE: run of colors)
    let mut pi = 0usize;
    while off < data.len() && pi < pix_count && pi < 1920 * 1080 {
        let b = data[off];
        off += 1;
        if b & 0x80 != 0 {
            // Run of identical pixels
            let run = (b & 0x7f) as usize + 1;
            if off + 3 > data.len() { break; }
            let r = data[off]; let g = data[off + 1]; let b2 = data[off + 2];
            off += 3;
            for _ in 0..run {
                if pi < pix_count {
                    pixels[pi * 3] = r; pixels[pi * 3 + 1] = g; pixels[pi * 3 + 2] = b2;
                    pi += 1;
                }
            }
        } else {
            // Raw pixel
            let run = (b & 0x7f) as usize + 1;
            for _ in 0..run {
                if off + 3 > data.len() || pi >= pix_count { break; }
                pixels[pi * 3] = data[off]; pixels[pi * 3 + 1] = data[off + 1]; pixels[pi * 3 + 2] = data[off + 2];
                off += 3; pi += 1;
            }
        }
    }

    let title = format!("Animation: {}", path);
    unsafe {
        let wid = create_window(title.as_ptr(), title.len() as u32, frame_width, frame_height);
        if wid >= 0 {
            update_window(wid, frame_width, frame_height, pixels.as_ptr(), pixels.len() as u32);
        }
    }
    println!("First frame decoded ({}x{})", frame_width, frame_height);
    true
}

// ── Hex fallback ───────────────────────────────────────────────

fn print_hex(path: &str, data: &[u8]) {
    println!("File: {}\nSize: {} bytes\nBinary data", path, data.len());
    let preview = data.len().min(256);
    for (i, chunk) in data[..preview].chunks(16).enumerate() {
        print!("{:08x}: ", i * 16);
        for b in chunk { print!("{:02x} ", b); }
        println!();
    }
}
