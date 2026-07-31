//! Native MP4 benchmark.
//!
//! This intentionally bypasses WASI, VFS, the window compositor, and Klog.
//! It measures the same MP4 container read, H.264 decode, and YUV->RGB work
//! used by the WASM viewer, so decoder cost can be separated from host drawing.

use mp4::{Mp4Reader, TrackType};
use std::fs::File;
use std::io::BufReader;
use std::time::Instant;
use z264::nal::{parse_annex_b, parse_avcc};
use z264::{Decoder, Frame};

const DEFAULT_PATH: &str =
    "/home/placeless/ビデオ/【東方】Bad Apple!! ＰＶ【影絵】 [FtutLA63Cp8].mp4";

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_PATH.to_string());
    let size = std::fs::metadata(&path).expect("MP4 stat failed").len();

    let header_start = Instant::now();
    let file = File::open(&path).expect("MP4 open failed");
    let mut reader = Mp4Reader::read_header(BufReader::new(file), size).expect("MP4 header failed");
    let header_time = header_start.elapsed();

    let (track_id, width, height, avcc, timescale) = reader
        .tracks()
        .values()
        .find(|track| track.track_type().ok() == Some(TrackType::Video))
        .and_then(|track| {
            let avc1 = track.trak.mdia.minf.stbl.stsd.avc1.as_ref()?;
            Some((
                track.track_id(),
                track.width() as u32,
                track.height() as u32,
                avc1.avcc.clone(),
                track.timescale(),
            ))
        })
        .expect("H.264 video track not found");
    let sample_count = reader.sample_count(track_id).expect("sample count failed");
    let length_size = usize::from(avcc.length_size_minus_one) + 1;

    let mut decoder = Decoder::new();
    let mut config_stream = Vec::new();
    for nal in avcc
        .sequence_parameter_sets
        .iter()
        .chain(avcc.picture_parameter_sets.iter())
    {
        config_stream.extend_from_slice(&[0, 0, 0, 1]);
        config_stream.extend_from_slice(&nal.bytes);
    }
    let config_start = Instant::now();
    for nal in parse_annex_b(&config_stream) {
        decoder
            .decode_nal(&nal)
            .expect("H.264 config decode failed");
    }
    let config_time = config_start.elapsed();

    let playback_start = Instant::now();
    let mut read_time = std::time::Duration::ZERO;
    let mut decode_time = std::time::Duration::ZERO;
    let mut convert_time = std::time::Duration::ZERO;
    let mut samples = 0u32;
    let mut nals = 0u64;
    let mut decoded_frames = 0u32;
    let mut rgb_bytes = 0usize;
    let mut rgb_buffer = Vec::new();

    for sample_id in 1..=sample_count {
        let read_start = Instant::now();
        let sample = reader
            .read_sample(track_id, sample_id)
            .expect("MP4 sample read failed")
            .expect("MP4 sample missing");
        read_time += read_start.elapsed();
        samples = samples.saturating_add(1);

        for nal in parse_avcc(&sample.bytes, length_size) {
            nals = nals.saturating_add(1);
            let decode_start = Instant::now();
            let output = decoder.decode_nal(&nal).expect("H.264 decode failed");
            decode_time += decode_start.elapsed();
            if let Some(frame) = output {
                decoded_frames = decoded_frames.saturating_add(1);
                let convert_start = Instant::now();
                yuv420_to_rgb(&frame, &mut rgb_buffer).expect("YUV frame conversion failed");
                convert_time += convert_start.elapsed();
                rgb_bytes = rgb_buffer.len();
                std::hint::black_box(&rgb_buffer);
            }
        }
    }
    if let Some(frame) = decoder.flush() {
        decoded_frames = decoded_frames.saturating_add(1);
        let convert_start = Instant::now();
        yuv420_to_rgb(&frame, &mut rgb_buffer).expect("YUV flush conversion failed");
        convert_time += convert_start.elapsed();
        rgb_bytes = rgb_buffer.len();
        std::hint::black_box(&rgb_buffer);
    }
    let playback_time = playback_start.elapsed();

    println!("file={path}");
    println!("size_bytes={size} resolution={width}x{height} timescale={timescale}");
    println!("samples={samples} nals={nals} decoded_frames={decoded_frames} rgb_bytes={rgb_bytes}");
    println!(
        "header_ms={:.3} config_ms={:.3}",
        ms(header_time),
        ms(config_time)
    );
    println!(
        "playback_ms={:.3} read_ms={:.3} decode_ms={:.3} convert_ms={:.3}",
        ms(playback_time),
        ms(read_time),
        ms(decode_time),
        ms(convert_time),
    );
    println!(
        "effective_fps={:.2} decode_share={:.1}% convert_share={:.1}% read_share={:.1}%",
        f64::from(decoded_frames) / playback_time.as_secs_f64(),
        100.0 * decode_time.as_secs_f64() / playback_time.as_secs_f64(),
        100.0 * convert_time.as_secs_f64() / playback_time.as_secs_f64(),
        100.0 * read_time.as_secs_f64() / playback_time.as_secs_f64(),
    );
}

fn ms(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn yuv420_to_rgb(frame: &Frame, rgb: &mut Vec<u8>) -> Option<()> {
    let width = usize::try_from(frame.width).ok()?;
    let height = usize::try_from(frame.height).ok()?;
    let y_len = width.checked_mul(height)?;
    let uv_width = width.div_ceil(2);
    let uv_height = height.div_ceil(2);
    let uv_len = uv_width.checked_mul(uv_height)?;
    if frame.y.len() < y_len || frame.u.len() < uv_len || frame.v.len() < uv_len {
        return None;
    }

    rgb.resize(y_len.checked_mul(3)?, 0);
    for source_y in 0..height {
        for source_x in 0..width {
            let yi = source_y * width + source_x;
            let ui = (source_y / 2) * uv_width + source_x / 2;
            let dst = (source_y * width + source_x) * 3;
            let yv = frame.y[yi] as i32;
            let uv = frame.u[ui] as i32 - 128;
            let vv = frame.v[ui] as i32 - 128;
            rgb[dst] = (yv + (359 * vv) / 256).clamp(0, 255) as u8;
            rgb[dst + 1] = (yv - (88 * uv + 183 * vv) / 256).clamp(0, 255) as u8;
            rgb[dst + 2] = (yv + (454 * uv) / 256).clamp(0, 255) as u8;
        }
    }
    Some(())
}
