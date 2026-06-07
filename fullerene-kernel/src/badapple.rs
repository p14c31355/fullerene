//! Bad Apple!! — PC speaker playback with framebuffer animation.
//!
//! Implements the `badapple` shell command that plays a simplified
//! Bad Apple melody through the PC speaker while rendering a shadow-art
//! animation on the primary framebuffer.
//!
//! # Usage
//!
//! ```text
//! fullerene> badapple
//! ```
//!
//! The animation runs for ~60 seconds.  Press any key to abort early.

use alloc::format;

/// A single note in the melody: frequency (0 = rest) and duration in ms.
struct Note {
    freq_hz: u32,
    duration_ms: u32,
}

/// Simplified Bad Apple melody (main theme, looped).
///
/// Notes are taken from the iconic opening phrase, transposed to
/// a range the PC speaker can handle (82–1047 Hz).
const MELODY: &[Note] = &[
    // Phrase 1 — eerie 4-note descent
    Note { freq_hz: 392, duration_ms: 240 }, // G4
    Note { freq_hz: 349, duration_ms: 240 }, // F4
    Note { freq_hz: 330, duration_ms: 240 }, // E4
    Note { freq_hz: 294, duration_ms: 360 }, // D4

    Note { freq_hz: 0, duration_ms: 160 },

    // Phrase 2
    Note { freq_hz: 349, duration_ms: 200 }, // F4
    Note { freq_hz: 330, duration_ms: 200 }, // E4
    Note { freq_hz: 294, duration_ms: 200 }, // D4
    Note { freq_hz: 262, duration_ms: 300 }, // C4

    Note { freq_hz: 0, duration_ms: 120 },

    // Phrase 3 — rising
    Note { freq_hz: 330, duration_ms: 200 }, // E4
    Note { freq_hz: 349, duration_ms: 200 }, // F4
    Note { freq_hz: 392, duration_ms: 200 }, // G4
    Note { freq_hz: 440, duration_ms: 300 }, // A4

    Note { freq_hz: 0, duration_ms: 160 },

    // Phrase 4 — climax
    Note { freq_hz: 440, duration_ms: 280 }, // A4
    Note { freq_hz: 494, duration_ms: 280 }, // B4
    Note { freq_hz: 523, duration_ms: 280 }, // C5
    Note { freq_hz: 587, duration_ms: 400 }, // D5

    Note { freq_hz: 0, duration_ms: 240 },

    // Phrase 5 — descending lament
    Note { freq_hz: 523, duration_ms: 240 }, // C5
    Note { freq_hz: 494, duration_ms: 240 }, // B4
    Note { freq_hz: 440, duration_ms: 240 }, // A4
    Note { freq_hz: 392, duration_ms: 360 }, // G4

    Note { freq_hz: 0, duration_ms: 200 },

    // Phrase 6
    Note { freq_hz: 349, duration_ms: 200 }, // F4
    Note { freq_hz: 330, duration_ms: 200 }, // E4
    Note { freq_hz: 294, duration_ms: 200 }, // D4
    Note { freq_hz: 262, duration_ms: 480 }, // C4

    Note { freq_hz: 0, duration_ms: 400 },
];

/// Play the Bad Apple melody on PC speaker while animating the framebuffer.
///
/// Each note triggers a frame-buffer flash: the screen alternates between
/// white and black, creating a shadow-puppet effect.
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

    // Play the melody for up to 4 loops (~60 seconds)
    let total_loops = 4;
    let total_notes = MELODY.len() * total_loops;

    for note_idx in 0..total_notes {
        // Abort if a key is pressed
        if nitrogen::ps2::keyboard::input_available() {
            log::info!("Bad Apple aborted by user");
            // Flush any pending key presses
            while nitrogen::ps2::keyboard::read_char().is_some() {}
            break;
        }

        let note = &MELODY[note_idx % MELODY.len()];

        // ── Framebuffer animation ──────────────────────────
        // Phase of the current note (0 → 255) determines pixel op.
        let phase = (note_idx as u32 * 31) & 0xFF;
        let fill_color = if (phase & 0x40) != 0 {
            0xFFFFFFFFu32 // white flash
        } else {
            0x00000000u32 // black
        };

        // Draw a patterned overlay on the framebuffer.
        // Use a checkerboard pattern that shifts each frame.
        draw_pattern(fb_ptr, fb_width, fb_height, fb_len, fill_color, phase);

        // ── PC Speaker sound ──────────────────────────────
        crate::sound::pc_speaker_beep(note.freq_hz, note.duration_ms.max(50));

        // Small gap between notes for articulation.
        if note.duration_ms > 200 {
            for _ in 0..50_000 {
                core::hint::spin_loop();
            }
        }
    }

    // ── Restore desktop ───────────────────────────────────
    // Force a full redraw so the compositor paints the desktop
    // over our animation leftovers.
    solvent::force_desktop_redraw();

    log::info!("Bad Apple playback finished");
}

/// Draw a procedural shadow-puppet pattern onto the framebuffer.
///
/// The pattern creates a silhouette-like effect:
/// - Left side: dark silhouette circle (the "shadow puppet").
/// - Right side: inverted checkerboard that animates.
fn draw_pattern(
    fb_ptr: *mut u32,
    fb_width: usize,
    fb_height: usize,
    fb_len: usize,
    fill_color: u32,
    phase: u32,
) {
    let fb = unsafe { core::slice::from_raw_parts_mut(fb_ptr, fb_len) };
    let cx = (fb_width / 2) as i32;
    let cy = (fb_height / 2) as i32;

    // Radius of the silhouette circle pulsates with phase.
    let radius = (fb_height as i32 / 3) + ((phase as i32 & 15) - 8) * 2;

    for y in 0..fb_height {
        let row_off = y * fb_width;
        let dy = y as i32 - cy;

        for x in 0..fb_width {
            let dx = x as i32 - cx;
            let idx = row_off + x;

            if idx >= fb_len {
                continue;
            }

            // Silhouette circle (dark) on the left side
            let dist_sq = dx * dx + dy * dy;
            let in_circle = dist_sq < radius * radius;

            // Checkerboard overlay on the right
            let checker =
                (((x as u32) >> 3) ^ ((y as u32) >> 3) ^ (phase >> 3)) & 1;

            if in_circle {
                // Inside silhouette: invert the pixel
                let pixel = fb[idx];
                let inverted = !pixel;
                fb[idx] = inverted;
            } else if checker == 1 && x > fb_width / 2 {
                // Right-side checkerboard flash
                fb[idx] = fill_color;
            }
        }
    }
}