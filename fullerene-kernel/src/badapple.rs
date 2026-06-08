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

/// Write a u32 pixel to the framebuffer using volatile store.
/// The framebuffer may be WB-cached; `flush_gpu()` (sfence) commits it.
unsafe fn fb_write_u32(dst: *mut u32, value: u32) {
    core::ptr::write_volatile(dst, value);
}

/// All RLE-compressed frames.
static BADAPPLE_RLE: &[u8] = include_bytes!("badapple.rle");

/// PCM audio: 22 050 Hz, mono, signed 16‑bit little‑endian.
static BADAPPLE_PCM: &[u8] = include_bytes!("badapple.pcm");

const PCM_SAMPLE_RATE: u32 = 22050;
const PCM_BYTES_PER_SAMPLE: u32 = 2;
const PCM_BYTES_PER_SEC: u32 = PCM_SAMPLE_RATE * PCM_BYTES_PER_SAMPLE; // 44100

const RLE_MAGIC: &[u8; 4] = b"BARL";
const RLE_HDR_SIZE: usize = 16;

// ── RDTSC‑calibrated spin-loop timing ──────────────────────

/// Calibrate spin-loop speed: spins for ~100 ms and measures elapsed
/// RDTSC ticks.  Returns `(spins_per_real_ms)`.
fn calibrate_spins_per_ms() -> u64 {
    const CALIBRATION_MS: u64 = 100;
    // Safety net: if calibration fails, assume 400 000 spins/ms (typical QEMU).
    const FALLBACK_SPINS_PER_MS: u64 = 400_000;

    // Ensure RDTSC is well‑ordered.
    unsafe {
        core::arch::x86_64::_lfence();
    }

    let tsc_start: u64;
    let mut spins: u64 = 0;
    unsafe {
        tsc_start = core::arch::x86_64::_rdtsc();
    }
    loop {
        spins += 1;
        core::hint::spin_loop();
        let tsc_end = unsafe { core::arch::x86_64::_rdtsc() };
        let elapsed_tsc = tsc_end.wrapping_sub(tsc_start);
        // Assume CPU TSC ≈ 2.5 GHz → 1 ms ≈ 2.5M ticks.
        // 100 ms ≈ 250M ticks.
        if elapsed_tsc >= 250_000_000 {
            break;
        }
    }

    let per_ms = if CALIBRATION_MS > 0 { spins / CALIBRATION_MS } else { FALLBACK_SPINS_PER_MS };
    log::info!("Bad Apple: calibrated {} spins/ms ({} spins in ~{}ms)", per_ms, spins, CALIBRATION_MS);
    if per_ms == 0 { FALLBACK_SPINS_PER_MS } else { per_ms }
}

fn delay_ms(spins_per_ms: u64, ms: u64) {
    let spins = ms.saturating_mul(spins_per_ms);
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
    if fb_stride == 0 || fb_height == 0 {
        return;
    }

    let fw = rle_frame_w as usize;
    let fh = rle_frame_h as usize;

    if fw > fb_stride || fh > fb_height {
        return;
    }

    let ox = if fb_stride > fw { (fb_stride - fw) / 2 } else { 0 };
    let oy = if fb_height > fh { (fb_height - fh) / 2 } else { 0 };

    // Fill frame area with black (non-temporal store bypasses cache)
    let fb_ptr = fb.as_mut_ptr();
    for y in 0..fh {
        let row = (oy + y) * fb_stride + ox;
        if row + fw > fb.len() {
            continue;
        }
        for x in 0..fw {
            unsafe { fb_write_u32(fb_ptr.add(row + x), 0xFF000000); }
        }
    }

    // Walk RLE runs and paint pixels.
    // Format: [fill: u8][run_len: u16 LE] (confirmed via Python binary analysis).
    let mut pos: usize = 0;
    let mut cursor: usize = 0;

    while cursor + 3 <= rle_data.len() && pos < fw * fh {
        let fill = rle_data[cursor];
        let run_len = u16::from_le_bytes([rle_data[cursor + 1], rle_data[cursor + 2]]) as usize;
        cursor += 3;

        let run_len = run_len.min(fw * fh - pos);

        // Convert fill byte to a grayscale ARGB pixel.
        // fill=0x00 → black, fill=0xFF → white, etc.
        let gray = fill as u32;
        let pixel = 0xFF000000 | (gray << 16) | (gray << 8) | gray;

        let mut rem = run_len;
        let mut p = pos;
        while rem > 0 {
            let y = p / fw;
            let x = p % fw;
            unsafe {
                fb_write_u32(fb.as_mut_ptr().add((oy + y) * fb_stride + ox + x), pixel);
            }
            p += 1;
            rem -= 1;
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

    if frame_count == 0 {
        log::error!("Bad Apple: frame count is zero");
        return;
    }

    let n_frames = frame_count as usize;

    // Frame table: u16 LE × n_frames at offset 16.
    // Each entry = compressed byte size for that frame.
    // RLE data starts at 16 + n_frames * 2.
    let table_entry_size = 2usize;
    let data_start = RLE_HDR_SIZE.saturating_add(n_frames * table_entry_size);
    if data_start >= data.len() {
        log::error!("Bad Apple: RLE data start ({}) exceeds file size ({})", data_start, data.len());
        return;
    }

    // Build frame offset table.
    let mut frame_offsets = Vec::with_capacity(n_frames);
    let mut offset: u64 = data_start as u64;
    for i in 0..n_frames {
        let compressed_size = u16::from_le_bytes([
            data[RLE_HDR_SIZE + i * table_entry_size],
            data[RLE_HDR_SIZE + i * table_entry_size + 1],
        ]) as u64;
        frame_offsets.push(offset);
        offset = offset.saturating_add(compressed_size);
    }

    // Dump first few bytes of RLE data for diagnostics.
    let dbg_end = (data_start + 24).min(data.len());
    log::info!(
        "Bad Apple: data_start={}, first bytes: {:02x?}, {} frames",
        data_start,
        &data[data_start..dbg_end],
        n_frames,
    );

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

    if fb_ptr.is_null() || fb_stride_pixels == 0 || fb_height == 0 {
        log::error!("Bad Apple: invalid framebuffer parameters");
        return;
    }

    let fb_len = fb_stride_pixels * fb_height;
    let rle_w_usize = rle_w as usize;
    let rle_h_usize = rle_h as usize;
    if rle_w_usize > fb_stride_pixels || rle_h_usize > fb_height {
        log::error!("Bad Apple: RLE frame size exceeds framebuffer dimensions");
        return;
    }

    let fb = unsafe { core::slice::from_raw_parts_mut(fb_ptr, fb_len) };

    // Clear screen to black and flush.
    // Non-temporal stores: bypass cache, committed by sfence in flush_gpu().
    let fb_len = fb.len();
    let fb_ptr = fb.as_mut_ptr();
    for i in 0..fb_len {
        unsafe { fb_write_u32(fb_ptr.add(i), 0xFF000000); }
    }
    crate::graphics::flush_gpu();

    let spins_per_ms = calibrate_spins_per_ms();
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

    // Flush stale input to avoid Enter-key immediate abort.
    nitrogen::ps2::keyboard::flush_input();

    // ── Playback loop ─────────────────────────────────────
    let mut frame_idx: usize = 0;
    let mut pcm_offset: usize = 0;

    while frame_idx < n_frames && frame_idx < frame_offsets.len() {
        if nitrogen::ps2::keyboard::input_available() {
            log::info!("Bad Apple aborted by user");
            nitrogen::ps2::keyboard::flush_input();
            break;
        }

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