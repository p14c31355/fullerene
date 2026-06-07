//! Bad Apple!! — shadow-art video + HDA PCM audio playback
//!
//! Embeds 160×120 1‑bit RLE‑compressed frames and 22 050 Hz mono
//! s16le PCM via `include_bytes!`, renders frames to the framebuffer
//! while streaming audio through the Intel HD Audio controller.
//!
//! # RLE file format (`.rle`)
//!
//! ```text
//! Offset  Size  Description
//! 0       4     Magic: "BARL"
//! 4       4     Version: u32 LE
//! 8       4     Frame count: u32 LE
//! 12      2     Width: u16 LE
//! 14      2     Height: u16 LE
//! 16      N*4   Frame table: [compressed_size: u32 LE] × N
//! …       …     RLE data: [u16 LE run_len][u8 fill] …
//! ```

use alloc::vec::Vec;
use core::sync::atomic::Ordering;

/// All RLE-compressed frames.
static BADAPPLE_RLE: &[u8] = include_bytes!("badapple.rle");

/// PCM audio: 22 050 Hz, mono, signed 16‑bit little‑endian.
static BADAPPLE_PCM: &[u8] = include_bytes!("badapple.pcm");

const PCM_SAMPLE_RATE: u32 = 48000;
const PCM_BYTES_PER_SAMPLE: u32 = 2;
const PCM_BYTES_PER_SEC: u32 = PCM_SAMPLE_RATE * PCM_BYTES_PER_SAMPLE; // 96000

const RLE_MAGIC: &[u8; 4] = b"BARL";
const RLE_HDR_SIZE: usize = 16;

// ── Spin-loop calibration ──────────────────────────────────

fn calibrate_spin_loop() -> u64 {
    const TEST_SPINS: u64 = 3_000_000;   // ~2 s on typical hardware
    const DEFAULT_ITERS_PER_MS: u64 = 15_000;
    const TICK_DURATION_MS: u64 = 160;    // APIC div=16, init=1M, bus≈100 MHz

    let start_tick = crate::interrupts::TICK_COUNTER.load(Ordering::Relaxed);
    for _ in 0..TEST_SPINS {
        core::hint::spin_loop();
    }
    let end_tick = crate::interrupts::TICK_COUNTER.load(Ordering::Relaxed);
    let ticks_elapsed = end_tick.saturating_sub(start_tick);

    if ticks_elapsed == 0 {
        log::warn!("Bad Apple: tick counter not advancing, using default timing");
        return DEFAULT_ITERS_PER_MS;
    }

    let ms_elapsed = ticks_elapsed * TICK_DURATION_MS;
    TEST_SPINS / ms_elapsed.max(1)
}

fn delay_ms(spins_per_ms: u64, ms: u64) {
    let spins = ms * spins_per_ms;
    for _ in 0..spins {
        core::hint::spin_loop();
    }
}

// ── Frame rendering ────────────────────────────────────────

/// Decode one RLE frame and draw it to the framebuffer.
///
/// `fb_stride`: number of u32 pixels per scanline (stride in bytes / 4).
fn draw_rle_frame(
    fb: &mut [u32],
    fb_stride: usize,
    fb_height: usize,
    rle_frame_w: u16,
    rle_frame_h: u16,
    rle_data: &[u8],
) {
    let fw = rle_frame_w as usize;
    let fh = rle_frame_h as usize;
    let ox = if fb_stride > fw { (fb_stride - fw) / 2 } else { 0 };
    let oy = if fb_height > fh { (fb_height - fh) / 2 } else { 0 };

    // Fill frame area with black
    for y in 0..fh {
        let row = (oy + y) * fb_stride + ox;
        for x in 0..fw {
            fb[row + x] = 0xFF000000;
        }
    }

    // Walk RLE runs and paint white pixels
    let mut pos: usize = 0;
    let mut cursor: usize = 0;

    while cursor + 3 <= rle_data.len() && pos < fw * fh {
        let run_len = u16::from_le_bytes([rle_data[cursor], rle_data[cursor + 1]]) as usize;
        let fill = rle_data[cursor + 2];
        cursor += 3;

        let run_len = run_len.min(fw * fh - pos);

        if fill == 0xFF {
            let mut rem = run_len;
            let mut p = pos;
            while rem > 0 {
                let y = p / fw;
                let x = p % fw;
                fb[(oy + y) * fb_stride + ox + x] = 0xFFFFFFFF;
                p += 1;
                rem -= 1;
            }
        }
        pos += run_len;
    }
}

// ── Main playback ──────────────────────────────────────────

pub fn play_badapple() {
    log::info!("Bad Apple playback started");

    // ── Parse RLE header ──────────────────────────────────
    let data = BADAPPLE_RLE;
    if data.len() < RLE_HDR_SIZE || &data[0..4] != RLE_MAGIC {
        log::error!("Bad Apple: invalid RLE data");
        return;
    }
    let version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    if version != 1 {
        log::error!("Bad Apple: unsupported RLE version {}", version);
        return;
    }
    let frame_count = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    let rle_w = u16::from_le_bytes([data[12], data[13]]);
    let rle_h = u16::from_le_bytes([data[14], data[15]]);

    let n_frames = frame_count as usize;
    let table_start = RLE_HDR_SIZE;
    let table_end = table_start.saturating_add(n_frames * 4);
    if data.len() < table_end {
        log::error!("Bad Apple: RLE data truncated at frame table");
        return;
    }

    // Build frame offset table (alloc::vec on heap, not stack)
    let mut frame_offsets = Vec::with_capacity(n_frames);
    let mut offset: u64 = table_end as u64;
    for i in 0..n_frames {
        let compressed_size = u32::from_le_bytes([
            data[table_start + i * 4],
            data[table_start + i * 4 + 1],
            data[table_start + i * 4 + 2],
            data[table_start + i * 4 + 3],
        ]);
        frame_offsets.push(offset);
        offset = offset.saturating_add(compressed_size as u64);
    }

    // ── Acquire framebuffer ───────────────────────────────
    let (fb_ptr, fb_stride_pixels, fb_height) = {
        let renderer_lock = crate::graphics::PRIMARY_RENDERER.lock();
        let guard = renderer_lock;
        let r = match guard.as_ref() {
            Some(r) => r,
            None => {
                log::error!("Bad Apple: no primary renderer");
                return;
            }
        };
        let info = r.get_info();
        // For 32 bpp: pixel stride = byte stride / 4
        let stride_px = (info.stride as usize) / 4;
        (info.address as *mut u32, stride_px, info.height as usize)
    };
    // drop lock before long loop

    let fb_len = fb_stride_pixels * fb_height;
    let fb = unsafe { core::slice::from_raw_parts_mut(fb_ptr, fb_len) };

    // Clear screen to black and flush
    for pixel in fb.iter_mut() {
        *pixel = 0xFF000000;
    }
    crate::graphics::flush_gpu();

    let spins_per_ms = calibrate_spin_loop();
    let use_hda = crate::sound::hda_available();

    let pcm_total = BADAPPLE_PCM.len();
    let song_duration_ms = (pcm_total as u64 * 1000) / PCM_BYTES_PER_SEC as u64;
    let frame_interval_ms: u64 = song_duration_ms / (n_frames as u64).max(1);
    let pcm_bytes_per_frame = pcm_total / n_frames;

    log::info!(
        "Bad Apple: fb {}x{} (stride {} px), {} frames, {}x{} px rle, {:.1}s, {}ms/f, {} PCM B/f, HDA={}, {} spin/ms",
        fb_stride_pixels, fb_height, fb_stride_pixels,
        n_frames, rle_w, rle_h,
        song_duration_ms as f64 / 1000.0,
        frame_interval_ms,
        pcm_bytes_per_frame,
        use_hda,
        spins_per_ms,
    );

    let mut frame_idx: usize = 0;
    let mut pcm_offset: usize = 0;

    while frame_idx < n_frames {
        if nitrogen::ps2::keyboard::input_available() {
            log::info!("Bad Apple aborted by user");
            while nitrogen::ps2::keyboard::read_char().is_some() {}
            break;
        }

        // Draw frame
        if frame_idx < frame_offsets.len() {
            let off = frame_offsets[frame_idx] as usize;
            let next_off = if frame_idx + 1 < frame_offsets.len() {
                frame_offsets[frame_idx + 1] as usize
            } else {
                data.len()
            };
            if off < data.len() && next_off <= data.len() {
                draw_rle_frame(fb, fb_stride_pixels, fb_height, rle_w, rle_h, &data[off..next_off]);
                crate::graphics::flush_gpu();
            }
        }

        // Feed PCM
        if use_hda {
            let feed_start = pcm_offset;
            let feed_end = (feed_start + pcm_bytes_per_frame).min(pcm_total);
            if feed_end > feed_start {
                crate::sound::hda_feed_samples(&BADAPPLE_PCM[feed_start..feed_end]);
                pcm_offset = feed_end;
            }
        }

        frame_idx += 1;
        delay_ms(spins_per_ms, frame_interval_ms);
        core::hint::spin_loop();
    }

    // Drain silence (1 s worth)
    if use_hda {
        let silence = [0u8; 4096];
        for _ in 0..10 {
            crate::sound::hda_feed_samples(&silence);
            delay_ms(spins_per_ms, 100);
        }
    }

    solvent::force_desktop_redraw();
    log::info!(
        "Bad Apple playback finished ({} frames, {:.1}s)",
        frame_idx,
        frame_idx as f64 * frame_interval_ms as f64 / 1000.0,
    );
}