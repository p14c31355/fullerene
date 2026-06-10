//! Bad Apple!! — shadow-art video + HDA PCM audio playback.
//!
//! Now uses the RLE Player library and renders into a Solvent window
//! on the desktop instead of directly into the framebuffer.
//!
//! # Architecture
//!
//! ```text
//! badapple.rle + badapple.pcm (embedded)
//!     ↓
//! rle_player::RleFile::parse()
//!     ↓
//! solvent::create_window("Bad Apple", ...)
//!     ↓
//! decode frame → draw into window surface → invalidate → compositor redraws
//!     ↓
//! sound::hda_feed_pcm (audio)
//!     ↓
//! on exit → solvent::close_window()
//! ```
//!
//! This is Fullerene's first GUI application — playing a video
//! inside a window on the desktop, rather than taking over the
//! entire framebuffer.

use alloc::vec::Vec;
use core::arch::x86_64;

static BADAPPLE_RLE: &[u8] = include_bytes!("badapple.rle");
static BADAPPLE_PCM: &[u8] = include_bytes!("badapple.pcm");
const PCM_BYTES_PER_SEC: u32 = 96000; // 16-bit mono × 48000 Hz

/// Default threshold for hard black/white silhouette.
const THRESHOLD: u8 = 128;

/// Default window size for the Bad Apple player.
const WINDOW_WIDTH: u32 = 640;
const WINDOW_HEIGHT: u32 = 480;

/// Return a fixed TSC ticks-per-millisecond estimate (2.5 GHz).
fn calibrate_tsc_per_ms() -> u64 {
    2_500_000 // assume 2.5 GHz
}

pub fn play_badapple() {
    petroleum::serial::serial_log(format_args!("Bad Apple playback started (window mode)\n"));
    log::info!("Bad Apple playback started (window mode)");

    // ── Parse RLE file via rle_player library ──────────────
    let rle = match rle_player::RleFile::parse(BADAPPLE_RLE) {
        Ok(r) => r,
        Err(e) => {
            petroleum::serial::serial_log(format_args!("Bad Apple: RLE parse error: {:?}\n", e));
            log::error!("Bad Apple: RLE parse error: {:?}", e);
            return;
        }
    };

    let n = rle.frame_count as usize;
    let fw = rle.frame_width as u32;
    let fh = rle.frame_height as u32;
    if fw == 0 || fh == 0 {
        petroleum::serial::serial_log(format_args!("Bad Apple: zero frame size\n"));
        log::error!("Bad Apple: zero frame size");
        return;
    }

    // ── Create a window on the desktop ─────────────────────
    let win_id = match solvent::create_window("Bad Apple", 100, 80, WINDOW_WIDTH, WINDOW_HEIGHT) {
        Some(id) => {
            petroleum::serial::serial_log(format_args!("Bad Apple: window created (id={:?})\n", id));
            id
        }
        None => {
            petroleum::serial::serial_log(format_args!("Bad Apple: failed to create window\n"));
            log::error!("Bad Apple: failed to create window");
            return;
        }
    };

    // ── Force an immediate full desktop redraw so the
    //     window frame (title bar, border) appears at once ──
    solvent::force_desktop_redraw();
    crate::gui::render();

    // ── Decode buffer ──────────────────────────────────────
    let decode_total = rle.total_pixels();
    let mut decode_buf = alloc::vec![0u8; decode_total];

    // ── Compute letterbox / pillarbox draw region ──────────
    let (draw_w, draw_h, off_x, off_y) =
        rle_player::compute_letterbox(fw, fh, WINDOW_WIDTH, WINDOW_HEIGHT);
    petroleum::serial::serial_log(format_args!(
        "Bad Apple: src={}x{} dst={}x{} letterbox=({},{},{},{})\n",
        fw, fh, WINDOW_WIDTH, WINDOW_HEIGHT, draw_w, draw_h, off_x, off_y,
    ));

    // ── Timing calibration ─────────────────────────────────
    let tsc_per_ms = calibrate_tsc_per_ms();
    let pcm_total = BADAPPLE_PCM.len();
    let dur_ms = (pcm_total as u64 * 1000) / PCM_BYTES_PER_SEC as u64;
    let frame_interval_ms: u64 = dur_ms / (n as u64).max(1);
    let frame_interval_tsc = frame_interval_ms.saturating_mul(tsc_per_ms);
    // Audio feed every ~1 ms (paced by TSC)
    let audio_feed_tsc = tsc_per_ms;
    const HALF: usize = 16368;
    log::info!(
        "Bad Apple: {} frames, {:.1}s, {}ms/f, TSC/ms={}",
        n,
        dur_ms as f64 / 1000.0,
        frame_interval_ms,
        tsc_per_ms,
    );
    petroleum::serial::serial_log(format_args!(
        "Bad Apple: {} frames, {:.1}s, {}ms/f\n",
        n,
        dur_ms as f64 / 1000.0,
        frame_interval_ms,
    ));

    let use_hda = crate::sound::hda_available();
    nitrogen::ps2::keyboard::flush_input();

    // ── Pre‑fill DMA ring buffer both halves ───────────────
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
        crate::sound::hda_reset_prefill_tracking();
    }

    // ── Main playback loop ─────────────────────────────────
    let mut idx = 0usize;
    let mut last_audio_feed = unsafe { x86_64::_rdtsc() };
    while idx < n {
        // Abort on any keyboard input
        if nitrogen::ps2::keyboard::input_available()
            || nitrogen::ps2::keyboard::raw_key_available()
        {
            petroleum::serial::serial_log(format_args!("Bad Apple aborted at frame {}\n", idx));
            log::info!("Bad Apple aborted");
            nitrogen::ps2::keyboard::flush_input();
            break;
        }

        // ── Decode frame ───────────────────────────────
        if let Err(e) = rle.decode_frame(idx, &mut decode_buf) {
            log::error!("Bad Apple: decode error at frame {}: {:?}", idx, e);
            break;
        }

        // ── Draw into the window surface ────────────────
        let draw_ok = solvent::with_window_surface(win_id, |pixels, w, h| {
            rle_player::draw_decoded_frame(
                pixels,
                w,
                fw,
                fh,
                &decode_buf,
                off_x,
                off_y,
                draw_w.min(w),
                draw_h.min(h),
                THRESHOLD,
            );
            true
        });
        if draw_ok.is_none() {
            petroleum::serial::serial_log(format_args!(
                "Bad Apple: with_window_surface returned None at frame {}\n", idx,
            ));
            break;
        }

        // Trigger compositor redraw for this window
        solvent::invalidate_window(win_id);

        // Render the frame via gui::render (more direct than runtime_tick)
        crate::gui::render();

        idx += 1;

        // ── Frame pacing (TSC‑based busy‑wait) ─────────────
        let now = unsafe { x86_64::_rdtsc() };
        let frame_deadline = now.wrapping_add(frame_interval_tsc);
        while unsafe { x86_64::_rdtsc() } < frame_deadline {
            let now = unsafe { x86_64::_rdtsc() };
            if use_hda && now.wrapping_sub(last_audio_feed) >= audio_feed_tsc {
                last_audio_feed = now;
                crate::sound::hda_feed_pcm(BADAPPLE_PCM, &mut pcm_off, pcm_total, HALF);
            }
            crate::sound::hda_tick();
        }
    }

    // ── Drain remaining PCM ────────────────────────────────
    if use_hda {
        let drain_deadline =
            unsafe { x86_64::_rdtsc() }.wrapping_add(dur_ms.max(1000).saturating_mul(tsc_per_ms));
        while pcm_off < pcm_total && unsafe { x86_64::_rdtsc() } < drain_deadline {
            if nitrogen::ps2::keyboard::input_available()
                || nitrogen::ps2::keyboard::raw_key_available()
            {
                log::info!("Bad Apple aborted (during drain)");
                nitrogen::ps2::keyboard::flush_input();
                break;
            }
            crate::sound::hda_feed_pcm(BADAPPLE_PCM, &mut pcm_off, pcm_total, HALF);
            if crate::sound::hda_poll_block(Some(audio_feed_tsc)) {
                continue;
            }
            core::hint::spin_loop();
        }
        for _ in 0..4 {
            crate::sound::hda_feed_silence(HALF);
            crate::sound::hda_poll_delay(tsc_per_ms, 100);
        }
    }

    // ── Cleanup: close window, force desktop redraw ────────
    solvent::close_window(win_id);
    solvent::force_desktop_redraw();
    crate::gui::render();
    log::info!("Bad Apple finished ({} frames)", idx);
    petroleum::serial::serial_log(format_args!("Bad Apple finished ({} frames)\n", idx));
}
