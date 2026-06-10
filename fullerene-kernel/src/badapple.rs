//! Bad Apple!! — shadow-art video + HDA PCM audio playback.
//!
//! **Hybrid rendering**: the window frame (title bar, border) is drawn once
//! via the compositor.  After that, each frame is written directly into the
//! framebuffer at the window's client‑area coordinates, bypassing the
//! compositor for maximum frame rate.
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

/// Return a fixed TSC ticks-per-millisecond estimate (2.5 GHz).
fn calibrate_tsc_per_ms() -> u64 {
    2_500_000
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
            petroleum::serial::serial_log(format_args!("Bad Apple: window created (id={:?})\n", id));
            id
        }
        None => {
            petroleum::serial::serial_log(format_args!("Bad Apple: failed to create window\n"));
            return;
        }
    };

    // Draw the window frame (title bar, border) once via the compositor.
    // After this, video frames are drawn directly into the framebuffer.
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
        (i.address as *mut u32, (i.stride as usize) / 4, i.height as usize)
    };
    if fb_ptr.is_null() || fb_stride == 0 || fb_height == 0 {
        petroleum::serial::serial_log(format_args!("Bad Apple: invalid fb\n"));
        solvent::close_window(win_id);
        solvent::force_desktop_redraw();
        crate::gui::render();
        return;
    }

    // Compute the client-area rectangle in framebuffer coordinates.
    // Window position (x=100, y=80), plus title bar height.
    let fb_x = 100i32;
    let fb_y = 80 + TITLE_BAR_H; // skip title bar

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
    let tsc_per_ms = calibrate_tsc_per_ms();
    let pcm_total = BADAPPLE_PCM.len();
    let dur_ms = (pcm_total as u64 * 1000) / PCM_BYTES_PER_SEC as u64;
    let frame_interval_ms: u64 = dur_ms / (n as u64).max(1);
    let frame_interval_tsc = frame_interval_ms.saturating_mul(tsc_per_ms);
    let audio_feed_tsc = tsc_per_ms;
    const HALF: usize = 16368;
    petroleum::serial::serial_log(format_args!(
        "Bad Apple: {} frames, {:.1}s, {}ms/f\n",
        n, dur_ms as f64 / 1000.0, frame_interval_ms,
    ));

    let use_hda = crate::sound::hda_available();

    // ── Drain input (ignore any key buffered before playback) ──
    nitrogen::ps2::keyboard::flush_input();

    // ── Pre‑fill DMA ring buffer ───────────────────────────
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
        // Abort on any **ASCII** keyboard input — modifiers and
        // non‑printing keys are ignored so Super/Meta do not
        // accidentally stop playback.
        //
        // `read_char` returns `None` for non‑ASCII keys (arrows,
        // modifiers, function keys), so we only abort when an
        // actual character is available.
        if nitrogen::ps2::keyboard::input_available() {
            if let Some(_ch) = nitrogen::ps2::keyboard::read_char() {
                petroleum::serial::serial_log(format_args!("Bad Apple aborted\n"));
                log::info!("Bad Apple aborted");
                break;
            }
            // Modifier or non‑printing key — ignore, but consume.
        }

        // ── Decode frame ───────────────────────────────
        // Skips frames whose RLE data runs past EOF (original behaviour).
        let drawn = match rle.decode_frame(idx, &mut decode_buf) {
            Ok(d) => d,
            Err(rle_player::RleError::FrameOutOfRange) => break,
            Err(e) => {
                petroleum::serial::serial_log(format_args!(
                    "Bad Apple: decode error frame {}: {:?}\n", idx, e,
                ));
                break;
            }
        };

        // Only draw the frame when data was actually decoded.
        if drawn {
            // ── Draw directly into the framebuffer ─────────
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

        // ── Frame pacing + audio feed ──────────────────
        let frame_deadline = unsafe { x86_64::_rdtsc() }.wrapping_add(frame_interval_tsc);
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