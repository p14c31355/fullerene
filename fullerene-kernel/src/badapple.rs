//! Bad Apple!! — shadow-art video + HDA PCM audio playback
//!
//! Optimised rendering path:
//! - Pre‑computed scaling LUTs avoid per‑pixel divisions.
//! - Volatile framebuffer writes work correctly with all memory
//!   types (WB, WT, WC, UC).
//! - TSC‑based frame pacing replaces spin‑loop counting.
//! - Audio is fed at ~1 ms intervals instead of every spin iteration.
use alloc::vec::Vec;
use core::arch::x86_64;

static BADAPPLE_RLE: &[u8] = include_bytes!("badapple.rle");
static BADAPPLE_PCM: &[u8] = include_bytes!("badapple.pcm");
const PCM_BYTES_PER_SEC: u32 = 44100;
const RLE_MAGIC: &[u8; 4] = b"BARL";
const RLE_HDR_SIZE: usize = 16;

// ── Timing ──────────────────────────────────────────────────────

/// Calibrate TSC ticks per millisecond by spinning for ~100 ms.
fn calibrate_tsc_per_ms() -> u64 {
    const TSC_CAL_WINDOW: u64 = 250_000_000; // ≈ 100 ms @ 2.5 GHz
    const FALLBACK: u64 = 2_500_000; // assume 2.5 GHz
    unsafe {
        x86_64::_mm_lfence();
    }
    let start = unsafe { x86_64::_rdtsc() };
    loop {
        if unsafe { x86_64::_rdtsc() }.wrapping_sub(start) >= TSC_CAL_WINDOW {
            break;
        }
        core::hint::spin_loop();
    }
    let elapsed = unsafe { x86_64::_rdtsc() }.wrapping_sub(start);
    let per_ms = elapsed / 100;
    if per_ms == 0 { FALLBACK } else { per_ms }
}

// ── RLE decode ──────────────────────────────────────────────────

/// Decode one RLE frame into `buf` (length must be ≥ `total = fw*fh`).
#[inline]
fn decode_rle_frame(data: &[u8], buf: &mut [u8], total: usize) {
    let mut p = 0usize;
    let mut c = 0usize;
    while c + 3 <= data.len() && p < total {
        let fill = data[c];
        let rl = u16::from_le_bytes([data[c + 1], data[c + 2]]) as usize;
        c += 3;
        let end = (p + rl).min(total);
        buf[p..end].fill(fill);
        p = end;
    }
}

// ── Framebuffer drawing ─────────────────────────────────────────

/// Draw a decoded RLE frame to the framebuffer.
///
/// Uses non‑temporal stores (`_mm_stream_si32`) which bypass the CPU
/// cache and write directly to a WC buffer — safe and correct for all
/// memory types (WB, WT, WC, UC).  An `_mm_sfence()` in `flush_gpu()`
/// drains the WC buffer so the display controller sees every pixel.
///
/// # Safety
/// `fb` must point to a valid framebuffer of at least `fb_stride * fb_h` u32
/// elements.  `decode` must be at least `fw * fh` bytes.  `row_map` /
/// `col_map` must be pre‑computed scaling look‑up tables.
#[inline]
unsafe fn draw_decoded_frame(
    fb: *mut u32,
    fb_stride: usize,
    fb_h: usize,
    fw: usize,
    decode: &[u8],
    row_map: &[usize],
    col_map: &[usize],
) {
    for fy in 0..fb_h {
        let ry = row_map[fy];
        let src_row = &decode[ry * fw..];
        let row_off = fy * fb_stride;
        for fx in 0..fb_stride {
            let rx = col_map[fx];
            let g = src_row[rx] as u32;
            let pixel = 0xFF00_0000u32 | (g << 16) | (g << 8) | g;
            // non‑temporal store bypasses cache; _mm_sfence in flush_gpu drains WC buffer
            core::arch::x86_64::_mm_stream_si32(fb.add(row_off + fx) as *mut i32, pixel as i32);
        }
    }
}

pub fn play_badapple() {
    log::info!("Bad Apple playback started");
    let data = BADAPPLE_RLE;
    if data.len() < RLE_HDR_SIZE || &data[..4] != RLE_MAGIC {
        log::error!("Bad Apple: invalid RLE");
        return;
    }
    let ver = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    if ver != 1 {
        log::error!("Bad Apple: version {}", ver);
        return;
    }
    let fc = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    let fw = u16::from_le_bytes([data[12], data[13]]) as usize;
    let fh = u16::from_le_bytes([data[14], data[15]]) as usize;
    if fc == 0 {
        log::error!("Bad Apple: zero frames");
        return;
    }
    let n = fc as usize;
    let ds = RLE_HDR_SIZE + n * 2;
    if ds >= data.len() {
        log::error!("Bad Apple: RLE data exceeds file");
        return;
    }

    // ── Parse frame offsets ──────────────────────────────────
    let mut offs = Vec::with_capacity(n);
    let mut o: u64 = ds as u64;
    for i in 0..n {
        let cs =
            u16::from_le_bytes([data[RLE_HDR_SIZE + i * 2], data[RLE_HDR_SIZE + i * 2 + 1]]) as u64;
        offs.push(o);
        o = o.saturating_add(cs);
    }

    // ── Framebuffer info ─────────────────────────────────────
    let (fb, fbs, fbh) = {
        let g = crate::graphics::PRIMARY_RENDERER.lock();
        let r = match g.as_ref() {
            Some(r) => r,
            None => {
                log::error!("Bad Apple: no renderer");
                return;
            }
        };
        let i = r.get_info();
        (
            i.address as *mut u32,
            (i.stride as usize) / 4,
            i.height as usize,
        )
    };
    if fb.is_null() || fbs == 0 || fbh == 0 {
        log::error!("Bad Apple: invalid fb");
        return;
    }
    if fw == 0 || fh == 0 {
        log::error!("Bad Apple: zero frame size");
        return;
    }

    // ── Pre‑compute scaling LUTs ─────────────────────────────
    // Row map:  framebuffer y → RLE source row
    let mut row_map = alloc::vec![0usize; fbh];
    for fy in 0..fbh {
        row_map[fy] = (fy * fh / fbh).min(fh - 1);
    }
    // Column map: framebuffer x → RLE source column
    let mut col_map = alloc::vec![0usize; fbs];
    for fx in 0..fbs {
        col_map[fx] = (fx * fw / fbs).min(fw - 1);
    }

    // ── Decode buffer ────────────────────────────────────────
    let decode_total = fw * fh;
    let mut decode_buf = alloc::vec![0u8; decode_total];

    // ── Fill framebuffer black ───────────────────────────────
    let fb_len = fbs * fbh;
    unsafe {
        for i in 0..fb_len {
            core::ptr::write_volatile(fb.add(i), 0xFF00_0000u32);
        }
    }
    crate::graphics::flush_gpu();

    // ── Timing calibration ───────────────────────────────────
    let tsc_per_ms = calibrate_tsc_per_ms();
    let pcm_total = BADAPPLE_PCM.len();
    let dur_ms = (pcm_total as u64 * 1000) / PCM_BYTES_PER_SEC as u64;
    let frame_interval_ms: u64 = dur_ms / (n as u64).max(1);
    let frame_interval_tsc = frame_interval_ms.saturating_mul(tsc_per_ms);
    // Audio feed every ~1 ms (paced by TSC)
    let audio_feed_tsc = tsc_per_ms;
    const HALF: usize = 16368;
    log::info!(
        "Bad Apple: {} frames, {:.1}s, {}ms/f, TSC/ms={}",
        n,
        dur_ms as f64 / 1000.0,
        frame_interval_ms,
        tsc_per_ms,
    );

    let use_hda = crate::sound::hda_available();
    nitrogen::ps2::keyboard::flush_input();

    // ── Pre‑fill DMA ring buffer both halves ─────────────────
    let mut pcm_off: usize = 0;
    if use_hda {
        let e0 = HALF.min(pcm_total);
        if e0 > 0 {
            crate::sound::hda_write_direct(0, &BADAPPLE_PCM[..e0]);
            pcm_off = e0;
        }
        let e1 = (pcm_off + HALF).min(pcm_total);
        if e1 > pcm_off {
            crate::sound::hda_write_direct(HALF as u32, &BADAPPLE_PCM[pcm_off..e1]);
            pcm_off = e1;
        }
    }

    // ── Main playback loop ───────────────────────────────────
    let mut idx = 0usize;
    let mut last_audio_feed = unsafe { x86_64::_rdtsc() };
    while idx < n && idx < offs.len() {
        // Abort on keyboard input
        if nitrogen::ps2::keyboard::input_available() {
            log::info!("Bad Apple aborted");
            nitrogen::ps2::keyboard::flush_input();
            break;
        }

        // ── Render current frame ──
        let fo = offs[idx] as usize;
        let no = if idx + 1 < offs.len() {
            offs[idx + 1] as usize
        } else {
            data.len()
        };
        if fo < data.len() && no <= data.len() {
            decode_rle_frame(&data[fo..no], &mut decode_buf, decode_total);
            unsafe {
                draw_decoded_frame(fb, fbs, fbh, fw, &decode_buf, &row_map, &col_map);
            }
            crate::graphics::flush_gpu();
        }

        idx += 1;

        // ── Frame pacing (TSC‑based busy‑wait) ───────────────
        let frame_deadline = unsafe { x86_64::_rdtsc() }.wrapping_add(frame_interval_tsc);
        while unsafe { x86_64::_rdtsc() } < frame_deadline {
            // Feed audio at ~1 ms granularity
            let now = unsafe { x86_64::_rdtsc() };
            if use_hda && now.wrapping_sub(last_audio_feed) >= audio_feed_tsc {
                last_audio_feed = now;
                crate::sound::hda_feed_pcm(&BADAPPLE_PCM[pcm_off..], &mut pcm_off, pcm_total, HALF);
            }
            core::hint::spin_loop();
        }
    }

    // ── Drain remaining PCM ──────────────────────────────────
    if use_hda {
        let drain_deadline =
            unsafe { x86_64::_rdtsc() }.wrapping_add(dur_ms.max(1000).saturating_mul(tsc_per_ms));
        while pcm_off < pcm_total && unsafe { x86_64::_rdtsc() } < drain_deadline {
            crate::sound::hda_feed_pcm(&BADAPPLE_PCM[pcm_off..], &mut pcm_off, pcm_total, HALF);
            if crate::sound::hda_poll_block(Some(audio_feed_tsc)) {
                continue;
            }
            core::hint::spin_loop();
        }
        // Send silence to complete any in‑flight DMA buffer
        for _ in 0..4 {
            crate::sound::hda_feed_silence(HALF);
            crate::sound::hda_poll_delay(tsc_per_ms, 100);
        }
    }

    solvent::force_desktop_redraw();
    log::info!("Bad Apple finished ({} frames)", idx);
}
