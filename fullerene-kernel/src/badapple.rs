//! Bad Apple — RLE video + HDA PCM audio player. Uses AudioContext + FramebufferContext.
use crate::contexts::audio::AudioContext;
use alloc::vec::Vec;
use core::arch::x86_64;

static BADAPPLE_RLE: &[u8] = include_bytes!("badapple.rle");
static BADAPPLE_PCM: &[u8] = include_bytes!("badapple.pcm");
const PCM_BPS: u32 = 96000;
const THRESHOLD: u8 = 128;
const WIN_W: u32 = 640;
const WIN_H: u32 = 480;
const TITLE_BAR_H: i32 = 20;
const HALF: usize = 16368;

fn calibrate_tsc(audio: &AudioContext) -> u64 {
    const AUDIO_SZ: u64 = 32736;
    if !audio.hda_available() {
        return 3_000_000;
    }
    let t0 = unsafe { x86_64::_rdtsc() };
    let lpib0 = match audio.playback_progress() {
        Some(v) if v < AUDIO_SZ => v,
        _ => return 3_000_000,
    };
    let mut prev = lpib0;
    let start_progress;
    loop {
        if let Some(cur) = audio.playback_progress() {
            if cur >= AUDIO_SZ {
                return 3_000_000;
            }
            if cur != prev {
                start_progress = cur;
                break;
            }
            prev = cur;
        }
        if unsafe { x86_64::_rdtsc() }.wrapping_sub(t0) >= 300_000_000 {
            return 3_000_000;
        }
        core::hint::spin_loop();
    }
    const CALIB_HALF: u64 = AUDIO_SZ / 2;
    let t0 = unsafe { x86_64::_rdtsc() };
    let mut total = 0u64;
    prev = start_progress;
    loop {
        if let Some(cur) = audio.playback_progress() {
            if cur >= AUDIO_SZ {
                return 3_000_000;
            }
            total += if cur >= prev {
                cur - prev
            } else {
                (AUDIO_SZ - prev) + cur
            };
            prev = cur;
            if total >= CALIB_HALF {
                break;
            }
        }
        if unsafe { x86_64::_rdtsc() }.wrapping_sub(t0) >= 3_000_000_000 {
            return 3_000_000;
        }
        core::hint::spin_loop();
    }
    let ticks = unsafe { x86_64::_rdtsc() }.wrapping_sub(t0);
    let ms = CALIB_HALF.saturating_mul(1000) / 96_000;
    if ms == 0 {
        return 3_000_000;
    }
    let r = ticks / ms;
    if r < 100_000 || r > 10_000_000 {
        3_000_000
    } else {
        r
    }
}

pub fn play_badapple() {
    let rle = match rle_player::RleFile::parse(BADAPPLE_RLE) {
        Ok(r) => r,
        Err(e) => {
            log::info!("Bad Apple: parse {:?}", e);
            return;
        }
    };
    let n = rle.frame_count as usize;
    let fw = rle.frame_width as u32;
    let fh = rle.frame_height as u32;
    if fw == 0 || fh == 0 {
        return;
    }
    let win = match solvent::create_window("Bad Apple", 100, 80, WIN_W, WIN_H) {
        Some(id) => id,
        None => return,
    };
    solvent::force_desktop_redraw();
    crate::gui::render();
    solvent::suspend_rendering();

    let (fb_ptr, fb_stride, fb_height) = {
        let fb = crate::contexts::framebuffer::get_framebuffer().lock();
        match fb.as_ref().and_then(|f| f.renderer.as_ref()) {
            Some(r) => {
                let i = r.get_info();
                (
                    i.address as *mut u32,
                    (i.stride as usize) / 4,
                    i.height as usize,
                )
            }
            None => {
                solvent::resume_rendering();
                solvent::close_window(win);
                return;
            }
        }
    };
    if fb_ptr.is_null() {
        solvent::resume_rendering();
        solvent::close_window(win);
        return;
    }

    let (draw_w, draw_h, off_x, off_y) = rle_player::compute_letterbox(fw, fh, WIN_W, WIN_H);
    let pcm_total = BADAPPLE_PCM.len();
    let dur_ms = (pcm_total as u64 * 1000) / PCM_BPS as u64;
    let fi_ms = dur_ms / (n as u64).max(1);
    let use_hda = crate::contexts::audio::with_audio(|a| a.hda_available()).unwrap_or(false);
    nitrogen::ps2::keyboard::flush_input();

    let mut pcm_off: usize = 0;
    if use_hda {
        crate::contexts::audio::with_audio_mut(|a| {
            let e0 = HALF.min(pcm_total);
            if e0 > 0 {
                a.write_samples(0, &BADAPPLE_PCM[..e0]);
                pcm_off = e0;
            }
            let e1 = (pcm_off + HALF).min(pcm_total);
            if e1 > pcm_off {
                a.write_samples(HALF as u32, &BADAPPLE_PCM[pcm_off..e1]);
                pcm_off = e1;
            }
            a.reset_prefill_tracking();
        });
    }

    let tsc_per_ms = if use_hda {
        // Wait a little for DMA to start, then calibrate
        let start = unsafe { x86_64::_rdtsc() };
        while unsafe { x86_64::_rdtsc() }.wrapping_sub(start) < 300_000_000 {
            core::hint::spin_loop();
        }
        crate::contexts::audio::with_audio(|a| calibrate_tsc(a)).unwrap_or(3_000_000)
    } else {
        3_000_000
    };
    // Clamp frame interval: at least ~33 ms (30 fps) to avoid speedup
    let fi_tsc = fi_ms.saturating_mul(tsc_per_ms).max(tsc_per_ms.saturating_mul(33));
    let af_tsc = tsc_per_ms;

    let pcm_per_frame = pcm_total as u64 / (n as u64).max(1);
    let mut consumed: u64 = 0;
    let mut last_lpib: u64 = 0;
    let mut wraps: u64 = 0;
    let mut lpib_valid = false;
    if use_hda {
        if let Some(cur) = crate::contexts::audio::with_audio(|a| a.playback_progress()).flatten() {
            if cur < 32736 {
                last_lpib = cur;
                lpib_valid = true;
            }
        }
    }
    let audio_sz: u64 = 32736;
    let mut idx = 0usize;
    let mut last_af = unsafe { x86_64::_rdtsc() };
    let mut decode_buf = alloc::vec![0u8; rle.total_pixels()];

    'outer: while idx < n {
        // Key check: poll (works without IRQs) + queue (ASCII keys via IRQ)
        if nitrogen::ps2::keyboard::poll_key_hit() {
            break;
        }
        if nitrogen::ps2::keyboard::input_available() {
            nitrogen::ps2::keyboard::read_char();
            break;
        }
        if use_hda && lpib_valid {
            let target = (idx as u64 + 1).saturating_mul(pcm_per_frame);
            let ls = unsafe { x86_64::_rdtsc() };
            loop {
                if nitrogen::ps2::keyboard::poll_key_hit() {
                    break 'outer;
                }
                if nitrogen::ps2::keyboard::input_available() {
                    nitrogen::ps2::keyboard::read_char();
                    break 'outer;
                }
                if let Some(cur) =
                    crate::contexts::audio::with_audio(|a| a.playback_progress()).flatten()
                {
                    if cur >= audio_sz {
                        lpib_valid = false;
                        break;
                    }
                    if cur < last_lpib && (last_lpib - cur) > audio_sz / 2 {
                        wraps = wraps.saturating_add(1);
                    }
                    last_lpib = cur;
                    consumed = wraps.saturating_mul(audio_sz).saturating_add(cur);
                }
                let now = unsafe { x86_64::_rdtsc() };
                if now.wrapping_sub(last_af) >= af_tsc {
                    last_af = now;
                    crate::contexts::audio::with_audio_mut(|a| {
                        feed_pcm(a, &mut pcm_off, pcm_total)
                    });
                }
                if consumed >= target {
                    break;
                }
                if unsafe { x86_64::_rdtsc() }.wrapping_sub(ls) >= fi_tsc.saturating_mul(3) {
                    lpib_valid = false;
                    break;
                }
                nitrogen::hda::HdaController::tick_vm_exit();
            }
        }
        if !use_hda || !lpib_valid {
            if nitrogen::ps2::keyboard::poll_key_hit() {
                break;
            }
            if nitrogen::ps2::keyboard::input_available() {
                nitrogen::ps2::keyboard::read_char();
                break;
            }
            let fd = unsafe { x86_64::_rdtsc() }.wrapping_add(fi_tsc);
            while unsafe { x86_64::_rdtsc() } < fd {
                if nitrogen::ps2::keyboard::poll_key_hit() {
                    break 'outer;
                }
                if nitrogen::ps2::keyboard::input_available() {
                    nitrogen::ps2::keyboard::read_char();
                    break 'outer;
                }
                if use_hda && unsafe { x86_64::_rdtsc() }.wrapping_sub(last_af) >= af_tsc {
                    last_af = unsafe { x86_64::_rdtsc() };
                    crate::contexts::audio::with_audio_mut(|a| {
                        feed_pcm(a, &mut pcm_off, pcm_total)
                    });
                }
                core::hint::spin_loop();
            }
        }
        if rle.decode_frame(idx, &mut decode_buf).is_ok() {
            unsafe {
                rle_player::draw_decoded_frame(
                    core::slice::from_raw_parts_mut(fb_ptr, fb_stride * fb_height),
                    fb_stride as u32,
                    fw,
                    fh,
                    &decode_buf,
                    (100 + off_x as i32).max(0) as u32,
                    (80 + TITLE_BAR_H + off_y as i32).max(0) as u32,
                    draw_w,
                    draw_h,
                    THRESHOLD,
                );
            }
            crate::graphics::flush_gpu();
        }
        idx += 1;
    }

    if use_hda {
        let dd =
            unsafe { x86_64::_rdtsc() }.wrapping_add(dur_ms.max(1000).saturating_mul(tsc_per_ms));
        while pcm_off < pcm_total && unsafe { x86_64::_rdtsc() } < dd {
            if nitrogen::ps2::keyboard::poll_key_hit() {
                break;
            }
            if nitrogen::ps2::keyboard::input_available() {
                nitrogen::ps2::keyboard::read_char();
                break;
            }
            crate::contexts::audio::with_audio_mut(|a| {
                feed_pcm(a, &mut pcm_off, pcm_total);
                if a.poll_block(Some(af_tsc)) {}
            });
            core::hint::spin_loop();
        }
        crate::contexts::audio::with_audio_mut(|a| {
            for _ in 0..4 {
                if nitrogen::ps2::keyboard::poll_key_hit() {
                    break;
                }
                if nitrogen::ps2::keyboard::input_available() {
                    nitrogen::ps2::keyboard::read_char();
                    break;
                }
                a.feed_silence(HALF);
                a.poll_delay(tsc_per_ms, 100);
            }
        });
    }
    solvent::resume_rendering();
    solvent::close_window(win);
    solvent::force_desktop_redraw();
    crate::gui::render();
    log::info!("Bad Apple: {} frames", idx);
}

fn feed_pcm(a: &mut AudioContext, off: &mut usize, total: usize) {
    if *off >= total {
        return;
    }
    let end = (*off + HALF).min(total);
    let fed = a.feed_samples(&BADAPPLE_PCM[*off..end]);
    if fed > 0 {
        *off += fed;
    }
}
