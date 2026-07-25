use image::GenericImageView;

#[link(wasm_import_module = "fullerene")]
unsafe extern "C" {
    fn show_image(width: u32, height: u32, pixels_ptr: *const u8, pixels_len: u32) -> u32;
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

    if try_decode_image(&bytes) {
        return;
    }

    if is_mp4(&bytes) {
        print_mp4_info(path, &bytes);
        return;
    }

    if is_mp3(&bytes) {
        print_mp3_info(&bytes);
        return;
    }

    if is_wav(&bytes) {
        print_wav_info(&bytes);
        return;
    }

    if let Ok(text) = std::str::from_utf8(&bytes) {
        println!("{}", text);
        return;
    }

    print_hex_dump(path, &bytes);
}

// ── Image ───────────────────────────────────────────────────────

fn try_decode_image(data: &[u8]) -> bool {
    if let Ok(img) = image::load_from_memory(data) {
        let (w, h) = img.dimensions();
        let pixels = img.to_rgb8().into_raw();
        let _ = unsafe { show_image(w, h, pixels.as_ptr(), pixels.len() as u32) };
        true
    } else {
        false
    }
}

// ── MP4 ─────────────────────────────────────────────────────────

fn is_mp4(data: &[u8]) -> bool {
    // ftyp box at the start (possibly with 0-4 bytes of padding)
    for off in [0usize, 4] {
        if data.len() < off + 8 {
            continue;
        }
        let size = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
        if size as usize + off > data.len() {
            continue;
        }
        if &data[off + 4..off + 8] == b"ftyp" {
            return true;
        }
    }
    data.len() >= 4 && &data[4..8] == b"ftyp"
}

fn mp4_box_name(data: &[u8], offset: usize) -> Option<([u8; 4], usize)> {
    if offset + 8 > data.len() {
        return None;
    }
    let size32 = u32::from_be_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]]);
    let name: [u8; 4] = [data[offset + 4], data[offset + 5], data[offset + 6], data[offset + 7]];
    match size32 {
        0 => Some((name, data.len() - offset)),
        1 => {
            if offset + 16 > data.len() {
                return None;
            }
            let size64 = u64::from_be_bytes([
                data[offset + 8], data[offset + 9], data[offset + 10], data[offset + 11],
                data[offset + 12], data[offset + 13], data[offset + 14], data[offset + 15],
            ]);
            Some((name, size64 as usize))
        }
        s => Some((name, s as usize)),
    }
}

fn print_mp4_info(path: &str, data: &[u8]) {
    let mut major_brand = "unknown";
    let mut duration_secs: Option<f64> = None;
    let mut offset = 0usize;

    while offset + 8 <= data.len() {
        let Some((name, box_size)) = mp4_box_name(data, offset) else { break };
        let end = offset + box_size;
        if end > data.len() || box_size < 8 {
            break;
        }
        let payload = &data[offset + 8..end];

        match &name {
            b"ftyp" if payload.len() >= 4 => {
                major_brand = std::str::from_utf8(&payload[..4]).unwrap_or("????");
            }
            b"moov" => {
                let mut moov_off = 0usize;
                while moov_off + 8 <= payload.len() {
                    let Some((sub_name, sub_size)) = mp4_box_name(payload, moov_off) else { break };
                    let sub_end = moov_off + sub_size;
                    if sub_end > payload.len() || sub_size < 8 {
                        break;
                    }
                    if &sub_name == b"mvhd" && sub_size >= 32 {
                        let version = payload[moov_off + 8];
                        let timescale = u32::from_be_bytes(
                            payload[moov_off + 20..moov_off + 24].try_into().unwrap(),
                        ) as f64;
                        let duration = if version == 1 {
                            u64::from_be_bytes(
                                payload[moov_off + 32..moov_off + 40].try_into().unwrap(),
                            )
                        } else {
                            u32::from_be_bytes(
                                payload[moov_off + 28..moov_off + 32].try_into().unwrap(),
                            ) as u64
                        };
                        if timescale > 0.0 {
                            duration_secs = Some(duration as f64 / timescale);
                        }
                    }
                    moov_off = sub_end;
                }
            }
            _ => {}
        }
        offset = end;
        if offset > data.len().saturating_sub(8) {
            break;
        }
    }

    let duration_str = match duration_secs {
        Some(s) => {
            let total = s as u64;
            let h = total / 3600;
            let m = (total % 3600) / 60;
            let sec = total % 60;
            if h > 0 {
                format!("{}h{:02}m{:02}s", h, m, sec)
            } else {
                format!("{:02}m{:02}s", m, sec)
            }
        }
        None => String::from("unknown"),
    };

    let size_mb = data.len() as f64 / (1024.0 * 1024.0);
    println!("File: {}", path);
    println!("Type: MP4 video ({})", major_brand);
    println!("Size: {:.1} MB", size_mb);
    println!("Duration: {}", duration_str);
}

// ── MP3 ─────────────────────────────────────────────────────────

fn is_mp3(data: &[u8]) -> bool {
    // Check for ID3v2 header or sync frame at start
    if data.len() >= 10 && &data[..3] == b"ID3" {
        return true;
    }
    // Check for an MPEG frame sync
    find_mp3_frame(data, 0).is_some()
}

fn find_mp3_frame(data: &[u8], start: usize) -> Option<(u32, u32, u32, u32)> {
    // Returns (bitrate_kbps, sample_rate_hz, channels, samples_per_frame)
    const BITRATES: [u32; 16] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0];
    const SAMPLE_RATES: [u32; 4] = [44100, 48000, 32000, 0];

    for off in start..data.len().saturating_sub(4) {
        let h = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
        if h & 0xffe0_0000 != 0xffe0_0000 {
            continue;
        }
        let version = (h >> 19) & 0x3;
        let layer = (h >> 17) & 0x3;
        let bitrate_idx = ((h >> 12) & 0xf) as usize;
        let sr_idx = ((h >> 10) & 0x3) as usize;

        if version == 1 || layer != 1 || bitrate_idx == 0 || bitrate_idx == 15 || sr_idx == 3 {
            continue;
        }
        let bitrate = BITRATES[bitrate_idx];
        let sample_rate = match version {
            3 => SAMPLE_RATES[sr_idx],
            2 => SAMPLE_RATES[sr_idx] / 2,
            _ => SAMPLE_RATES[sr_idx] / 4,
        };
        let channels = if (h >> 6) & 0x3 == 3 { 1 } else { 2 };
        let samples = if version == 3 { 1152 } else { 576 };
        return Some((bitrate, sample_rate, channels, samples));
    }
    None
}

fn print_mp3_info(data: &[u8]) {
    let id3_size = if data.len() >= 10 && &data[..3] == b"ID3" {
        let size = ((data[6] as usize) << 21) | ((data[7] as usize) << 14)
            | ((data[8] as usize) << 7) | (data[9] as usize);
        size + 10
    } else {
        0
    };

    let mut total_frames = 0u64;
    let mut total_samples = 0u64;
    let mut sample_rate = 0u32;
    let mut channels = 0u32;
    let mut bitrate = 0u32;
    let mut offset = id3_size;

    while let Some((br, sr, ch, spf)) = find_mp3_frame(data, offset) {
        if total_frames == 0 {
            bitrate = br;
            sample_rate = sr;
            channels = ch;
        }
        total_frames += 1;
        total_samples += spf as u64;
        // Estimate frame size
        let frame_size = (144_000 * br / sr).max(1) as usize;
        offset += frame_size;
        if total_frames > 10000 {
            break;
        }
    }

    let duration_secs = if sample_rate > 0 {
        total_samples as f64 / sample_rate as f64
    } else {
        0.0
    };

    println!("Type: MP3 audio");
    println!("Bitrate: {} kbps", bitrate);
    println!("Sample rate: {} Hz", sample_rate);
    println!("Channels: {}", if channels == 1 { "mono" } else { "stereo" });
    println!("Duration: {:.1} s", duration_secs);
    println!("Frames: {}", total_frames);
}

// ── WAV ─────────────────────────────────────────────────────────

fn is_wav(data: &[u8]) -> bool {
    data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WAVE"
}

fn print_wav_info(data: &[u8]) {
    let channels = u16::from_le_bytes([data[22], data[23]]);
    let sample_rate = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
    let bits_per_sample = u16::from_le_bytes([data[34], data[35]]);

    let mut data_size = 0u32;
    let mut off = 36;
    while off + 8 <= data.len() {
        let chunk_size = u32::from_le_bytes([data[off + 4], data[off + 5], data[off + 6], data[off + 7]]);
        if &data[off..off + 4] == b"data" {
            data_size = chunk_size;
            break;
        }
        off += 8 + chunk_size as usize;
    }

    let duration = if sample_rate > 0 && channels > 0 && bits_per_sample > 0 {
        data_size as f64 / (channels as f64 * (bits_per_sample as f64 / 8.0) * sample_rate as f64)
    } else {
        0.0
    };

    println!("Type: WAV audio");
    println!("Channels: {}", channels);
    println!("Sample rate: {} Hz", sample_rate);
    println!("Bits: {}-bit", bits_per_sample);
    println!("Data size: {} bytes", data_size);
    println!("Duration: {:.1} s", duration);
}

// ── Hex dump fallback ───────────────────────────────────────────

fn print_hex_dump(path: &str, data: &[u8]) {
    println!("File: {}", path);
    println!("Size: {} bytes", data.len());
    println!("Binary data — cannot display as text, image, or known media format.");
    let preview = data.len().min(256);
    for (i, chunk) in data[..preview].chunks(16).enumerate() {
        print!("{:08x}: ", i * 16);
        for b in chunk {
            print!("{:02x} ", b);
        }
        println!();
    }
}
