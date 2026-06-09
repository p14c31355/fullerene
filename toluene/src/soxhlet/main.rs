//! soxhlet — Bad Apple video converter for Fullerene OS
//!
//! Converts the Bad Apple MP4 into two assets used by the kernel:
//!
//! 1. `badapple.rle` — 160×120 1‑bit RLE‑compressed frames
//! 2. `badapple.pcm` — mono 48 kHz 16‑bit signed PCM audio

use clap::Parser;
use std::io::{self, BufWriter, Read, Write};
use std::process::{Command, Stdio};

/// Target frame dimensions (width × height) after scaling.
const FRAME_W: u32 = 160;
const FRAME_H: u32 = 120;
const PIXELS_PER_FRAME: usize = (FRAME_W * FRAME_H) as usize;

/// RLE file magic: "BARL"
const RLE_MAGIC: &[u8; 4] = b"BARL";
const RLE_VERSION: u32 = 1;

/// Audio: mono 48 kHz 16‑bit signed PCM
const AUDIO_SAMPLE_RATE: u32 = 48000;

#[derive(Parser)]
struct Args {
    /// Path to the input MP4 file
    input: String,

    /// Output path for badapple.rle (default: badapple.rle)
    #[arg(long, default_value = "badapple.rle")]
    rle_out: String,

    /// Output path for badapple.pcm (default: badapple.pcm)
    #[arg(long, default_value = "badapple.pcm")]
    pcm_out: String,
}

fn main() -> io::Result<()> {
    let args = Args::parse();

    // ── Step 1: Extract PCM audio ─────────────────────────────
    eprintln!("[1/3] Extracting PCM audio → {}", args.pcm_out);
    extract_pcm_audio(&args.input, &args.pcm_out)?;

    // ── Step 2: Extract raw 1‑bit frames via ffmpeg pipe ──────
    eprintln!(
        "[2/3] Extracting & RLE‑compressing frames → {}",
        args.rle_out
    );
    convert_video_to_rle(&args.input, &args.rle_out)?;

    eprintln!("[3/3] Done.");
    Ok(())
}

/// Use ffmpeg to resample the input audio track to mono 16‑bit PCM.
fn extract_pcm_audio(input: &str, output: &str) -> io::Result<()> {
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            input,
            "-vn",
            "-ac",
            "1",
            "-ar",
            &AUDIO_SAMPLE_RATE.to_string(),
            "-sample_fmt",
            "s16",
            "-f",
            "s16le",
            "-y",
            output,
        ])
        .status()?;

    if !status.success() {
        return Err(io::Error::other("ffmpeg audio extraction failed"));
    }
    Ok(())
}

/// Convert video to RLE-compressed 1‑bit frames.
fn convert_video_to_rle(input: &str, output: &str) -> io::Result<()> {
    let mut ffmpeg = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            input,
            "-an",
            "-vf",
            &format!("scale={}:{},format=gray", FRAME_W, FRAME_H),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    let stdout = ffmpeg
        .stdout
        .take()
        .expect("failed to capture ffmpeg stdout");

    let mut reader = io::BufReader::with_capacity(PIXELS_PER_FRAME * 256, stdout);
    let mut frame_buf = vec![0u8; PIXELS_PER_FRAME];
    let mut rle_frames: Vec<Vec<u8>> = Vec::with_capacity(7000);

    loop {
        match reader.read_exact(&mut frame_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        let rle = rle_encode_1bit(&frame_buf);
        rle_frames.push(rle);
    }

    let status = ffmpeg.wait()?;
    if !status.success() {
        return Err(io::Error::other("ffmpeg frame extraction failed"));
    }

    eprintln!("    Frames extracted: {}", rle_frames.len());

    // ── Write output file ─────────────────────────────────
    let mut out = BufWriter::new(std::fs::File::create(output)?);

    // Header
    out.write_all(RLE_MAGIC)?;
    out.write_all(&RLE_VERSION.to_le_bytes())?;
    out.write_all(&(rle_frames.len() as u32).to_le_bytes())?;
    out.write_all(&(FRAME_W as u16).to_le_bytes())?;
    out.write_all(&(FRAME_H as u16).to_le_bytes())?;

    // Frame data: [compressed_size: u32 LE][RLE data]
    for rle in &rle_frames {
        let size = rle.len() as u32;
        out.write_all(&size.to_le_bytes())?;
        out.write_all(rle)?;
    }

    out.flush()?;

    let total_rle_bytes: usize = rle_frames.iter().map(|f| f.len()).sum();
    eprintln!(
        "    RLE total: {} bytes ({:.1}% of raw {} bytes)",
        total_rle_bytes,
        total_rle_bytes as f64 / (rle_frames.len() * PIXELS_PER_FRAME) as f64 * 100.0,
        rle_frames.len() * PIXELS_PER_FRAME,
    );

    Ok(())
}

/// Encode a 160×120 greyscale frame into RLE runs.
fn rle_encode_1bit(grey: &[u8]) -> Vec<u8> {
    assert_eq!(grey.len(), PIXELS_PER_FRAME);

    let mut out = Vec::with_capacity(512);
    let mut run_start: usize = 0;
    let mut current_val: bool = grey[0] >= 128;

    for i in 1..PIXELS_PER_FRAME {
        let val = grey[i] >= 128;
        if val != current_val {
            push_run(&mut out, i - run_start, current_val);
            run_start = i;
            current_val = val;
        }
    }
    push_run(&mut out, PIXELS_PER_FRAME - run_start, current_val);
    out
}

/// Push one RLE run: [u16 LE count][u8 fill].
fn push_run(out: &mut Vec<u8>, count: usize, is_white: bool) {
    let mut remaining = count;
    while remaining > 0 {
        let chunk = remaining.min(65535u32 as usize) as u16;
        out.extend_from_slice(&chunk.to_le_bytes());
        out.push(if is_white { 0xFF } else { 0x00 });
        remaining -= chunk as usize;
    }
}
