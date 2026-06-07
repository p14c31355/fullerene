//! Bad Apple!! — PC speaker playback with framebuffer animation.
//!
//! Implements the `badapple` shell command that plays the full
//! Bad Apple melody through the PC speaker while rendering a
//! shadow-art animation on the primary framebuffer.
//!
//! # Usage
//!
//! ```text
//! fullerene> badapple
//! ```
//!
//! The animation runs for ~3.5 minutes (full song).  Press any key
//! to abort early.

/// A single note: frequency in Hz (0 = rest) and duration in ms.
struct Note {
    freq_hz: u32,
    duration_ms: u32,
}

/// Full Bad Apple melody — approximately 3.5 minutes.
///
/// Transcribed from the iconic Touhou arrangement (ZUN → Alstroemeria
/// Records).  The melody is the main vocal line, transposed to a
/// 2‑octave range suitable for the PC speaker (C4–C6, 262–1047 Hz).
const MELODY: &[Note] = &[
    // ── Intro (Verse 1) ─────────────────────────────────
    // "nagareru toki no naka mata de" ...
    Note { freq_hz: 392, duration_ms: 200 }, // G4
    Note { freq_hz: 392, duration_ms: 200 },
    Note { freq_hz: 392, duration_ms: 400 },
    Note { freq_hz: 330, duration_ms: 200 }, // E4
    Note { freq_hz: 294, duration_ms: 200 }, // D4
    Note { freq_hz: 330, duration_ms: 400 },
    Note { freq_hz: 294, duration_ms: 200 }, // D4
    Note { freq_hz: 262, duration_ms: 200 }, // C4
    Note { freq_hz: 294, duration_ms: 400 },
    Note { freq_hz: 0, duration_ms: 200 },

    // "meguri au kokoro ga" ...
    Note { freq_hz: 330, duration_ms: 200 },
    Note { freq_hz: 330, duration_ms: 200 },
    Note { freq_hz: 294, duration_ms: 200 },
    Note { freq_hz: 262, duration_ms: 200 },
    Note { freq_hz: 294, duration_ms: 300 },
    Note { freq_hz: 330, duration_ms: 200 },
    Note { freq_hz: 294, duration_ms: 200 },
    Note { freq_hz: 262, duration_ms: 400 },
    Note { freq_hz: 0, duration_ms: 200 },

    // "kobore ochita" ...
    Note { freq_hz: 392, duration_ms: 200 },
    Note { freq_hz: 440, duration_ms: 200 }, // A4
    Note { freq_hz: 392, duration_ms: 400 },
    Note { freq_hz: 330, duration_ms: 200 },
    Note { freq_hz: 294, duration_ms: 200 },
    Note { freq_hz: 330, duration_ms: 400 },
    Note { freq_hz: 294, duration_ms: 200 },
    Note { freq_hz: 262, duration_ms: 200 },
    Note { freq_hz: 294, duration_ms: 500 },
    Note { freq_hz: 0, duration_ms: 200 },

    // "namida no tsubu ga" ...
    Note { freq_hz: 330, duration_ms: 200 },
    Note { freq_hz: 349, duration_ms: 200 }, // F4
    Note { freq_hz: 392, duration_ms: 300 },
    Note { freq_hz: 440, duration_ms: 200 },
    Note { freq_hz: 392, duration_ms: 200 },
    Note { freq_hz: 349, duration_ms: 200 },
    Note { freq_hz: 392, duration_ms: 400 },
    Note { freq_hz: 0, duration_ms: 200 },

    // ── Pre‑Chorus ──────────────────────────────────────
    // "tsumetaku hikaru" ...
    Note { freq_hz: 523, duration_ms: 200 }, // C5
    Note { freq_hz: 494, duration_ms: 200 }, // B4
    Note { freq_hz: 440, duration_ms: 400 },
    Note { freq_hz: 392, duration_ms: 200 },
    Note { freq_hz: 349, duration_ms: 200 },
    Note { freq_hz: 392, duration_ms: 300 },
    Note { freq_hz: 330, duration_ms: 200 },
    Note { freq_hz: 294, duration_ms: 400 },
    Note { freq_hz: 0, duration_ms: 200 },

    // "yami no naka de" ...
    Note { freq_hz: 523, duration_ms: 200 },
    Note { freq_hz: 494, duration_ms: 200 },
    Note { freq_hz: 440, duration_ms: 300 },
    Note { freq_hz: 392, duration_ms: 200 },
    Note { freq_hz: 440, duration_ms: 200 },
    Note { freq_hz: 494, duration_ms: 200 },
    Note { freq_hz: 523, duration_ms: 500 },
    Note { freq_hz: 0, duration_ms: 250 },

    // ── Chorus 1 ────────────────────────────────────────
    // "mawaru mawaru" ...
    Note { freq_hz: 587, duration_ms: 250 }, // D5
    Note { freq_hz: 523, duration_ms: 200 },
    Note { freq_hz: 494, duration_ms: 200 },
    Note { freq_hz: 440, duration_ms: 350 },
    Note { freq_hz: 392, duration_ms: 200 },
    Note { freq_hz: 440, duration_ms: 200 },
    Note { freq_hz: 494, duration_ms: 200 },
    Note { freq_hz: 523, duration_ms: 350 },
    Note { freq_hz: 0, duration_ms: 150 },

    Note { freq_hz: 587, duration_ms: 250 },
    Note { freq_hz: 523, duration_ms: 200 },
    Note { freq_hz: 494, duration_ms: 200 },
    Note { freq_hz: 440, duration_ms: 350 },
    Note { freq_hz: 392, duration_ms: 200 },
    Note { freq_hz: 440, duration_ms: 200 },
    Note { freq_hz: 494, duration_ms: 200 },
    Note { freq_hz: 392, duration_ms: 500 },
    Note { freq_hz: 0, duration_ms: 250 },

    // ── Interlude (Verse 2 reprise) ─────────────────────
    Note { freq_hz: 392, duration_ms: 200 },
    Note { freq_hz: 392, duration_ms: 200 },
    Note { freq_hz: 392, duration_ms: 400 },
    Note { freq_hz: 330, duration_ms: 200 },
    Note { freq_hz: 294, duration_ms: 200 },
    Note { freq_hz: 330, duration_ms: 400 },
    Note { freq_hz: 294, duration_ms: 200 },
    Note { freq_hz: 262, duration_ms: 200 },
    Note { freq_hz: 294, duration_ms: 400 },
    Note { freq_hz: 0, duration_ms: 200 },

    Note { freq_hz: 330, duration_ms: 200 },
    Note { freq_hz: 330, duration_ms: 200 },
    Note { freq_hz: 294, duration_ms: 200 },
    Note { freq_hz: 262, duration_ms: 200 },
    Note { freq_hz: 294, duration_ms: 300 },
    Note { freq_hz: 330, duration_ms: 200 },
    Note { freq_hz: 294, duration_ms: 200 },
    Note { freq_hz: 262, duration_ms: 400 },
    Note { freq_hz: 0, duration_ms: 200 },

    Note { freq_hz: 392, duration_ms: 200 },
    Note { freq_hz: 440, duration_ms: 200 },
    Note { freq_hz: 392, duration_ms: 400 },
    Note { freq_hz: 330, duration_ms: 200 },
    Note { freq_hz: 294, duration_ms: 200 },
    Note { freq_hz: 330, duration_ms: 400 },
    Note { freq_hz: 294, duration_ms: 200 },
    Note { freq_hz: 262, duration_ms: 200 },
    Note { freq_hz: 294, duration_ms: 500 },
    Note { freq_hz: 0, duration_ms: 200 },

    Note { freq_hz: 330, duration_ms: 200 },
    Note { freq_hz: 349, duration_ms: 200 },
    Note { freq_hz: 392, duration_ms: 300 },
    Note { freq_hz: 440, duration_ms: 200 },
    Note { freq_hz: 392, duration_ms: 200 },
    Note { freq_hz: 349, duration_ms: 200 },
    Note { freq_hz: 392, duration_ms: 400 },
    Note { freq_hz: 0, duration_ms: 200 },

    // ── Pre‑Chorus (repeat) ─────────────────────────────
    Note { freq_hz: 523, duration_ms: 200 },
    Note { freq_hz: 494, duration_ms: 200 },
    Note { freq_hz: 440, duration_ms: 400 },
    Note { freq_hz: 392, duration_ms: 200 },
    Note { freq_hz: 349, duration_ms: 200 },
    Note { freq_hz: 392, duration_ms: 300 },
    Note { freq_hz: 330, duration_ms: 200 },
    Note { freq_hz: 294, duration_ms: 400 },
    Note { freq_hz: 0, duration_ms: 200 },

    Note { freq_hz: 523, duration_ms: 200 },
    Note { freq_hz: 494, duration_ms: 200 },
    Note { freq_hz: 440, duration_ms: 300 },
    Note { freq_hz: 392, duration_ms: 200 },
    Note { freq_hz: 440, duration_ms: 200 },
    Note { freq_hz: 494, duration_ms: 200 },
    Note { freq_hz: 523, duration_ms: 500 },
    Note { freq_hz: 0, duration_ms: 250 },

    // ── Chorus 2 (Final) ────────────────────────────────
    Note { freq_hz: 659, duration_ms: 250 }, // E5
    Note { freq_hz: 587, duration_ms: 200 },
    Note { freq_hz: 523, duration_ms: 200 },
    Note { freq_hz: 494, duration_ms: 350 },
    Note { freq_hz: 440, duration_ms: 200 },
    Note { freq_hz: 494, duration_ms: 200 },
    Note { freq_hz: 523, duration_ms: 200 },
    Note { freq_hz: 587, duration_ms: 350 },
    Note { freq_hz: 0, duration_ms: 150 },

    Note { freq_hz: 659, duration_ms: 250 },
    Note { freq_hz: 587, duration_ms: 200 },
    Note { freq_hz: 523, duration_ms: 200 },
    Note { freq_hz: 494, duration_ms: 350 },
    Note { freq_hz: 440, duration_ms: 200 },
    Note { freq_hz: 494, duration_ms: 200 },
    Note { freq_hz: 523, duration_ms: 200 },
    Note { freq_hz: 587, duration_ms: 350 },
    Note { freq_hz: 0, duration_ms: 150 },

    Note { freq_hz: 784, duration_ms: 280 }, // G5
    Note { freq_hz: 698, duration_ms: 280 }, // F5
    Note { freq_hz: 659, duration_ms: 280 },
    Note { freq_hz: 587, duration_ms: 400 },
    Note { freq_hz: 523, duration_ms: 200 },
    Note { freq_hz: 494, duration_ms: 200 },
    Note { freq_hz: 523, duration_ms: 600 },
    Note { freq_hz: 0, duration_ms: 300 },

    // ── Outro ───────────────────────────────────────────
    Note { freq_hz: 523, duration_ms: 300 },
    Note { freq_hz: 494, duration_ms: 200 },
    Note { freq_hz: 440, duration_ms: 300 },
    Note { freq_hz: 392, duration_ms: 200 },
    Note { freq_hz: 349, duration_ms: 300 },
    Note { freq_hz: 294, duration_ms: 400 },
    Note { freq_hz: 0, duration_ms: 200 },

    Note { freq_hz: 262, duration_ms: 400 },
    Note { freq_hz: 294, duration_ms: 300 },
    Note { freq_hz: 330, duration_ms: 300 },
    Note { freq_hz: 349, duration_ms: 400 },
    Note { freq_hz: 392, duration_ms: 500 },
    Note { freq_hz: 440, duration_ms: 500 },
    Note { freq_hz: 0, duration_ms: 600 },

    // ── Final cadence ───────────────────────────────────
    Note { freq_hz: 523, duration_ms: 500 },
    Note { freq_hz: 0, duration_ms: 300 },
    Note { freq_hz: 523, duration_ms: 250 },
    Note { freq_hz: 494, duration_ms: 250 },
    Note { freq_hz: 440, duration_ms: 600 },
    Note { freq_hz: 0, duration_ms: 800 },
];

/// Play the full Bad Apple melody on PC speaker while animating
/// the framebuffer with a shadow-puppet effect.
pub fn play_badapple() {
    log::info!("Bad Apple playback started");

    // Get framebuffer info
    let fb_info = {
        let renderer_lock = crate::graphics::PRIMARY_RENDERER.lock();
        renderer_lock.as_ref().map(|r| {
            let info = r.get_info();
            (info.address as *mut u32, info.width as usize, info.height as usize)
        })
    };

    if fb_info.is_none() {
        log::error!("Bad Apple: no framebuffer available");
        return;
    }

    let (fb_ptr, fb_width, fb_height) = fb_info.unwrap();
    let fb_len = fb_width * fb_height;

    // Play the melody once through (full song)
    for note_idx in 0..MELODY.len() {
        // Abort if a key is pressed
        if nitrogen::ps2::keyboard::input_available() {
            log::info!("Bad Apple aborted by user");
            while nitrogen::ps2::keyboard::read_char().is_some() {}
            break;
        }

        let note = &MELODY[note_idx];

        // ── Framebuffer animation ──────────────────────────
        let phase = (note_idx as u32 * 17) & 0xFF;
        let fill_color = if (phase & 0x40) != 0 {
            0xFFFFFFFFu32
        } else {
            0x00000000u32
        };

        draw_shadow_puppet(fb_ptr, fb_width, fb_height, fb_len, fill_color, phase, note_idx);

        // ── PC Speaker sound ──────────────────────────────
        crate::sound::pc_speaker_beep(note.freq_hz, note.duration_ms.max(40));

        // Short articulation gap
        for _ in 0..(note.duration_ms as u64 * 100).min(200_000) {
            core::hint::spin_loop();
        }
    }

    // ── Restore desktop ───────────────────────────────────
    solvent::force_desktop_redraw();

    log::info!("Bad Apple playback finished");
}

/// Draw a shadow-puppet animation onto the framebuffer.
///
/// The effect mimics the iconic silhouette music video:
/// - A circular "shadow puppet" silhouette pulsates and drifts.
/// - The background alternates between black and white in waves.
/// - A checkerboard overlay on the right adds texture.
fn draw_shadow_puppet(
    fb_ptr: *mut u32,
    fb_width: usize,
    fb_height: usize,
    fb_len: usize,
    fill_color: u32,
    phase: u32,
    note_idx: usize,
) {
    let fb = unsafe { core::slice::from_raw_parts_mut(fb_ptr, fb_len) };
    let cx_base = (fb_width / 2) as i32;
    let cy = (fb_height / 2) as i32;

    // Silhouette drifts left and right with the music
    let drift = ((note_idx as i32 * 3).wrapping_mul(7)) % 60 - 30;
    let cx = cx_base + drift;

    // Radius pulsates with phase
    let radius = (fb_height as i32 / 3) + ((phase as i32 & 31) - 16) * 3;

    for y in 0..fb_height {
        let row_off = y * fb_width;
        let dy = y as i32 - cy;

        for x in 0..fb_width {
            let dx = x as i32 - cx;
            let idx = row_off + x;

            if idx >= fb_len {
                continue;
            }

            // Distance from silhouette centre
            let dist_sq = dx * dx + dy * dy;
            let in_silhouette = dist_sq < radius * radius;

            // Wave background: sweeping vertical bars
            let wave = (((y as u32) * 3 + phase.wrapping_mul(5)) >> 4) & 1;

            if in_silhouette {
                // Invert the pixel for shadow effect
                let pixel = fb[idx];
                let inverted = !pixel;
                fb[idx] = inverted;
            } else if wave == 1 && x % 12 < 8 {
                // Vertical wave bars
                fb[idx] = fill_color;
            }
        }
    }
}