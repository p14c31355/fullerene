//! Bad Apple!! — shadow-art video + HDA PCM audio playback.
//!
//! **Hybrid rendering**: the window frame (title bar, border) is drawn once
//! via the compositor.  After that, each frame is written directly into the
//! framebuffer at the window's client‑area coordinates, bypassing the
//! compositor for maximum frame rate.
//!
//! **TSC calibration**: measures actual CPU clock against the HDA DMA
//! progress (LPIB register) so frame pacing is accurate regardless of
//! host CPU speed.
//!
//! On exit the desktop is fully repainted via `force_desktop_redraw()` +
//! `gui::render()`.

use alloc::vec::Vec;
use core::arch::x86_64;

static BADAPPLE_RLE: &[u8] = include_bytes!("badapple.rle");
static BADAPPLE_PCM: &[u8] = include_bytes!("badapple.pcm");
const PCM_BYTES_PER_SEC: u32 = 96000; // 16-bit mono × 48000 Hz
const THRESHOLD: u8 = 128;

/// Default window size (client area) for the Bad Apple player.
const WINDOW_WIDTH: u32 = 640;
const WINDOW_HEIGHT: u32 = 480;

/// Title bar height — must match `lattice::compositor::TITLE_BAR_HEIGHT`.
const TITLE_BAR_H: i32 = 20;

/// Calibrate TSC ticks-per-millisecond using the HDA DMA progress
/// (LPIB register).  Falls back to 3 GHz when HDA is absent or
/// the DMA engine is not advancing (common on real hardware where
/// the codec or stream setup may not complete).
fn calibrate_tsc_per_ms() -> u64 {
    if !crate::sound::hda_available() {
        return 3_000_000;
    }
    // Quick sanity: if LPIB doesn't move at all within ~100 ms
    // the DMA engine is stalled — bail out immediately.
    let t0 = unsafe { x86_64::_rdtsc() };
    let mut lpib = crate::sound::hda_playback_progress().unwrap_or(0);
    let settle = t0.wrapping_add(300_000_000); // ~100 ms at 3 GHz
    loop {
        if let Some(cur) = crate::sound::hda_playback_progress() {
            if cur != lpib {
                break; // LPIB is moving
            }
            lpib = cur;
        }
        if unsafe { x86_64::_rdtsc() } > settle {
            return 3_000_000; // stalled
        }
        core::hint::spin_loop();
    }
    // LPIB is advancing — measure half‑buffer worth of progress.
    let half = 16352u64;
    let t0 = unsafe { x86_64::_rdtsc() };
    let deadline = t0.wrapping_add(3_000_000_000); // ~1 s at 3 GHz
    let mut prev = lpib;
    let mut total = 0u64;
    loop {
        if let Some(cur) = crate::sound::hda_playback_progress() {
            // LPIB wraps at audio_sz (32704); detect wrap via
            // subtraction underflow and accumulate correctly.
            let delta = if cur >= prev {
                cur - prev
            } else {
                (32704u64 - prev) + cur
            };
            total += delta;
            prev = cur;
            if total >= half {
                break;
            }
        }
        if unsafe { x86_64::_rdtsc() } > deadline {
            return 3_000_000;
        }
        core::hint::spin_loop();
    }
    let ticks = unsafe { x86_64::_rdtsc() }.wrapping_sub(t0);
    let ms = half.saturating_mul(1000) / 96_000; // half bytes @ 96000 B/s → ms
    if ms == 0 {
        return 3_000_000;
    }
    ticks / ms
}

pub fn play_badapple() {
    petroleum::serial::serial_log(format_args!("Bad Apple playback started (hybrid mode)\n"));
    log::info!("Bad Apple playback started (hybrid mode)");

    // ── Parse RLE ──────────────────────────────────────────
    let rle = match rle_player::RleFile::parse(BADAPPLE_RLE) {
        Ok(r) => r,
        Err(e) => {
            petroleum::serial::serial_log(format_args!("Bad Apple: parse error: {:?}\n", e));
            return;
        }
    };
    let n = rle.frame_count as usize;
    let fw = rle.frame_width as u32;
    let fh = rle.frame_height as u32;
    if fw == 0 || fh == 0 {
        petroleum::serial::serial_log(format_args!("Bad Apple: zero frame size\n"));
        return;
    }

    // ── Create a window on the desktop ─────────────────────
    let win_id = match solvent::create_window("Bad Apple", 100, 80, WINDOW_WIDTH, WINDOW_HEIGHT) {
        Some(id) => {
            petroleum::serial::serial_log(format_args!(
                "Bad Apple: window created (id={:?})\n",
                id
            ));
            id
        }
        None => {
            petroleum::serial::serial_log(format_args!("Bad Apple: failed to create window\n"));
            return;
        }
    };

    // Draw the window frame (title bar, border) once via the compositor.
    solvent::force_desktop_redraw();
    crate::gui::render();

    // ── Framebuffer info (for direct writes) ────────────────
    let (fb_ptr, fb_stride, fb_height) = {
        let g = crate::graphics::PRIMARY_RENDERER.lock();
        let r = match g.as_ref() {
            Some(r) => r,
            None => {
                petroleum::serial::serial_log(format_args!("Bad Apple: no renderer\n"));
                solvent::close_window(win_id);
                solvent::force_desktop_redraw();
                crate::gui::render();
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
    if fb_ptr.is_null() || fb_stride == 0 || fb_height == 0 {
        petroleum::serial::serial_log(format_args!("Bad Apple: invalid fb\n"));
        solvent::close_window(win_id);
        solvent::force_desktop_redraw();
        crate::gui::render();
        return;
    }

    // Compute the client-area rectangle in framebuffer coordinates.
    let fb_x = 100i32;
    let fb_y = 80 + TITLE_BAR_H;

    // ── Decode buffer ──────────────────────────────────────
    let decode_total = rle.total_pixels();
    let mut decode_buf = alloc::vec![0u8; decode_total];

    // ── Letterbox region ───────────────────────────────────
    let (draw_w, draw_h, off_x, off_y) =
        rle_player::compute_letterbox(fw, fh, WINDOW_WIDTH, WINDOW_HEIGHT);
    petroleum::serial::serial_log(format_args!(
        "Bad Apple: src={}x{} clip={}x{} letterbox=({},{},{},{})\n",
        fw, fh, WINDOW_WIDTH, WINDOW_HEIGHT, draw_w, draw_h, off_x, off_y,
    ));

    // ── Timing ─────────────────────────────────────────────
    let pcm_total = BADAPPLE_PCM.len();
    let dur_ms = (pcm_total as u64 * 1000) / PCM_BYTES_PER_SEC as u64;
    let frame_interval_ms: u64 = dur_ms / (n as u64).max(1);
    const HALF: usize = 16368;

    let use_hda = crate::sound::hda_available();

    // ── Drain input ────────────────────────────────────────
    nitrogen::ps2::keyboard::flush_input();

    // ── Pre‑fill DMA ring buffer both halves ───────────────
    // (also triggers hda_init via hda_write_direct)
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

    // ── TSC calibration (HDA LPIB‑based) ──────────────────
    // Must run AFTER hda_init (triggered by hda_write_direct above)
    // so that the DMA engine is running and LPIB is advancing.
    let tsc_per_ms = if use_hda {
        calibrate_tsc_per_ms()
    } else {
        3_000_000 // fallback: 3 GHz
    };
    petroleum::serial::serial_log(format_args!(
        "Bad Apple: TSC/ms={} (~{:.1} GHz)\n",
        tsc_per_ms,
        tsc_per_ms as f64 / 1_000_000.0,
    ));

    let frame_interval_tsc = frame_interval_ms.saturating_mul(tsc_per_ms);
    let audio_feed_tsc = tsc_per_ms;
    petroleum::serial::serial_log(format_args!(
        "Bad Apple: {} frames, {:.1}s, {}ms/f, {}tsc/f\n",
        n,
        dur_ms as f64 / 1000.0,
        frame_interval_ms,
        frame_interval_tsc,
    ));

    // ── Main playback loop (LPIB‑synced with TSC watchdog) ─
    //
    // Primary clock: HDA LPIB (96000 bytes/sec hardware rate).
    // Fallback: TSC‑based pacing when HDA is absent or DMA stalls.
    //
    // `consumed` tracks total bytes the HDA controller has read
    // from the DMA ring buffer since playback started.
    let pcm_per_frame = (pcm_total as u64) / (n as u64).max(1);
    let mut consumed: u64 = 0;
    let mut last_lpib: u64 = 0;
    let mut lpib_valid = false;
    if use_hda {
        if let Some(cur) = crate::sound::hda_playback_progress() {
            last_lpib = cur;
            lpib_valid = true;
        }
    }
    let audio_sz: u64 = 32704;
    let mut idx = 0usize;
    let mut last_audio_feed = unsafe { x86_64::_rdtsc() };
    'outer: while idx < n {
        // Abort on ASCII key.
        if nitrogen::ps2::keyboard::input_available() {
            if let Some(_ch) = nitrogen::ps2::keyboard::read_char() {
                petroleum::serial::serial_log(format_args!("Bad Apple aborted\n"));
                log::info!("Bad Apple aborted");
                break;
            }
        }

        if use_hda && lpib_valid {
            // ── LPIB‑synced wait ─────────────────────────
            let target = (idx as u64 + 1).saturating_mul(pcm_per_frame);
            let deadline =
                unsafe { x86_64::_rdtsc() }.wrapping_add(frame_interval_tsc.saturating_mul(3));
            loop {
                if nitrogen::ps2::keyboard::input_available() {
                    if let Some(_ch) = nitrogen::ps2::keyboard::read_char() {
                        petroleum::serial::serial_log(format_args!("Bad Apple aborted\n"));
                        log::info!("Bad Apple aborted");
                        break 'outer;
                    }
                }
                // Update consumed-bytes counter from LPIB.
                if let Some(cur) = crate::sound::hda_playback_progress() {
                    let delta = cur.wrapping_sub(last_lpib);
                    last_lpib = cur;
                    if delta < audio_sz {
                        consumed = consumed.saturating_add(delta);
                    }
                }
                // Feed audio.
                let now = unsafe { x86_64::_rdtsc() };
                if now.wrapping_sub(last_audio_feed) >= audio_feed_tsc {
                    last_audio_feed = now;
                    crate::sound::hda_feed_pcm(BADAPPLE_PCM, &mut pcm_off, pcm_total, HALF);
                }
                if consumed >= target {
                    break;
                }
                // TSC watchdog: fall back if LPIB isn't advancing.
                if unsafe { x86_64::_rdtsc() } > deadline {
                    petroleum::serial::serial_log(format_args!(
                        "Bad Apple: LPIB stalled, using TSC\n"
                    ));
                    lpib_valid = false;
                    break;
                }
                crate::sound::hda_tick();
            }
        }

        if !use_hda || !lpib_valid {
            // ── TSC‑based pacing ──────────────────────────
            if nitrogen::ps2::keyboard::input_available() {
                if let Some(_ch) = nitrogen::ps2::keyboard::read_char() {
                    break;
                }
            }
            let frame_deadline = unsafe { x86_64::_rdtsc() }.wrapping_add(frame_interval_tsc);
            while unsafe { x86_64::_rdtsc() } < frame_deadline {
                let now = unsafe { x86_64::_rdtsc() };
                if use_hda && now.wrapping_sub(last_audio_feed) >= audio_feed_tsc {
                    last_audio_feed = now;
                    crate::sound::hda_feed_pcm(BADAPPLE_PCM, &mut pcm_off, pcm_total, HALF);
                }
                core::hint::spin_loop();
            }
        }

        // ── Decode & draw frame ─────────────────────────
        let drawn = match rle.decode_frame(idx, &mut decode_buf) {
            Ok(d) => d,
            Err(rle_player::RleError::FrameOutOfRange) => break,
            Err(e) => {
                petroleum::serial::serial_log(format_args!(
                    "Bad Apple: decode error frame {}: {:?}\n",
                    idx, e,
                ));
                break;
            }
        };

        if drawn {
            unsafe {
                rle_player::draw_decoded_frame(
                    core::slice::from_raw_parts_mut(fb_ptr, fb_stride * fb_height),
                    fb_stride as u32,
                    fw,
                    fh,
                    &decode_buf,
                    (fb_x + off_x as i32).max(0) as u32,
                    (fb_y + off_y as i32).max(0) as u32,
                    draw_w,
                    draw_h,
                    THRESHOLD,
                );
            }
            crate::graphics::flush_gpu();
        }

        idx += 1;
    }

    // ── Drain remaining PCM ────────────────────────────────
    if use_hda {
        let drain_deadline =
            unsafe { x86_64::_rdtsc() }.wrapping_add(dur_ms.max(1000).saturating_mul(tsc_per_ms));
        while pcm_off < pcm_total && unsafe { x86_64::_rdtsc() } < drain_deadline {
            if nitrogen::ps2::keyboard::input_available() {
                if nitrogen::ps2::keyboard::read_char().is_some() {
                    break;
                }
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

    // ── Restore desktop ────────────────────────────────────
    solvent::close_window(win_id);
    solvent::force_desktop_redraw();
    crate::gui::render();
    log::info!("Bad Apple finished ({} frames)", idx);
    petroleum::serial::serial_log(format_args!("Bad Apple finished ({} frames)\n", idx));
}
