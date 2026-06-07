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
//! The animation runs for the duration of the melody.  Press any key
//! to abort early.

use core::sync::atomic::Ordering;

// ── Note structure ──────────────────────────────────────────────

/// A single note: frequency in Hz (0 = rest) and duration in ms.
struct Note {
    freq_hz: u32,
    duration_ms: u32,
}

impl Note {
    const fn new(freq_hz: u32, duration_ms: u32) -> Self {
        Self {
            freq_hz,
            duration_ms,
        }
    }
}

// ── Helper aliases for readability ──────────────────────────────
const R: u32 = 0; // rest
#[allow(unused)]
const C4: u32 = 262;
const D4: u32 = 294;
const E4: u32 = 330;
const F4: u32 = 349;
const G4: u32 = 392;
const A4: u32 = 440;
const B4: u32 = 494;
const C5: u32 = 523;
const D5: u32 = 587;
const E5: u32 = 659;
const F5: u32 = 698;
const G5: u32 = 784;
const A5: u32 = 880;

// ── Note durations (ms) — calibrated for ~138 BPM ───────────────
const S: u32 = 110; // sixteenth
const E: u32 = 220; // eighth
const Q: u32 = 440; // quarter
const H: u32 = 880; // half
const W: u32 = 1760; // whole

/// Full Bad Apple melody.
///
/// Transcribed from the iconic Touhou arrangement (ZUN → Alstroemeria
/// Records).  The melody is the main vocal line, transposed to a
/// 2‑octave range suitable for the PC speaker (C4–A5, 262–880 Hz).
///
/// Structure:
///   Intro / Verse 1 → Pre‑Chorus → Chorus 1 →
///   Verse 2 → Pre‑Chorus → Chorus 2 →
///   Bridge → Final Chorus → Outro
#[rustfmt::skip]
const MELODY: &[Note] = &[
    // ════════════════════════════════════════════════════════════
    // Section 1: Intro / Verse 1
    // ════════════════════════════════════════════════════════════
    Note::new(G4, E), Note::new(G4, E), Note::new(G4, Q),
    Note::new(E4, E), Note::new(D4, E), Note::new(E4, Q),
    Note::new(D4, E), Note::new(C4, E), Note::new(D4, Q), Note::new(R, S),

    Note::new(E4, E), Note::new(E4, E), Note::new(D4, E),
    Note::new(C4, E), Note::new(D4, Q), Note::new(E4, E),
    Note::new(D4, E), Note::new(C4, Q), Note::new(R, S),

    Note::new(G4, E), Note::new(A4, E), Note::new(G4, Q),
    Note::new(E4, E), Note::new(D4, E), Note::new(E4, Q),
    Note::new(D4, E), Note::new(C4, E), Note::new(D4, H), Note::new(R, E),

    Note::new(E4, E), Note::new(F4, E), Note::new(G4, Q),
    Note::new(A4, E), Note::new(G4, E), Note::new(F4, E),
    Note::new(G4, Q), Note::new(R, S),

    // ════════════════════════════════════════════════════════════
    // Section 2: Pre‑Chorus 1
    // ════════════════════════════════════════════════════════════
    Note::new(C5, E), Note::new(B4, E), Note::new(A4, Q),
    Note::new(G4, E), Note::new(F4, E), Note::new(G4, Q),
    Note::new(E4, E), Note::new(D4, Q), Note::new(R, S),

    Note::new(C5, E), Note::new(B4, E), Note::new(A4, Q),
    Note::new(G4, E), Note::new(A4, E), Note::new(B4, E),
    Note::new(C5, H), Note::new(R, E),

    // ════════════════════════════════════════════════════════════
    // Section 3: Chorus 1
    // ════════════════════════════════════════════════════════════
    Note::new(D5, E), Note::new(C5, E), Note::new(B4, E),
    Note::new(A4, Q), Note::new(G4, E), Note::new(A4, E),
    Note::new(B4, E), Note::new(C5, Q), Note::new(R, S),

    Note::new(D5, E), Note::new(C5, E), Note::new(B4, E),
    Note::new(A4, Q), Note::new(G4, E), Note::new(A4, E),
    Note::new(B4, E), Note::new(G4, H), Note::new(R, E),

    Note::new(D5, E), Note::new(C5, E), Note::new(B4, E),
    Note::new(A4, Q), Note::new(G4, E), Note::new(A4, E),
    Note::new(B4, E), Note::new(C5, Q), Note::new(R, S),

    Note::new(D5, E), Note::new(C5, E), Note::new(B4, E),
    Note::new(E5, Q), Note::new(D5, E), Note::new(C5, E),
    Note::new(B4, E), Note::new(A4, H), Note::new(R, E),

    // ════════════════════════════════════════════════════════════
    // Section 4: Verse 2 (reprise)
    // ════════════════════════════════════════════════════════════
    Note::new(G4, E), Note::new(G4, E), Note::new(G4, Q),
    Note::new(E4, E), Note::new(D4, E), Note::new(E4, Q),
    Note::new(D4, E), Note::new(C4, E), Note::new(D4, Q), Note::new(R, S),

    Note::new(E4, E), Note::new(E4, E), Note::new(D4, E),
    Note::new(C4, E), Note::new(D4, Q), Note::new(E4, E),
    Note::new(D4, E), Note::new(C4, Q), Note::new(R, S),

    Note::new(G4, E), Note::new(A4, E), Note::new(G4, Q),
    Note::new(E4, E), Note::new(D4, E), Note::new(E4, Q),
    Note::new(D4, E), Note::new(C4, E), Note::new(D4, H), Note::new(R, E),

    Note::new(E4, E), Note::new(F4, E), Note::new(G4, Q),
    Note::new(A4, E), Note::new(G4, E), Note::new(F4, E),
    Note::new(G4, Q), Note::new(R, S),

    // ════════════════════════════════════════════════════════════
    // Section 5: Pre‑Chorus 2
    // ════════════════════════════════════════════════════════════
    Note::new(C5, E), Note::new(B4, E), Note::new(A4, Q),
    Note::new(G4, E), Note::new(F4, E), Note::new(G4, Q),
    Note::new(E4, E), Note::new(D4, Q), Note::new(R, S),

    Note::new(C5, E), Note::new(B4, E), Note::new(A4, Q),
    Note::new(G4, E), Note::new(A4, E), Note::new(B4, E),
    Note::new(C5, H), Note::new(R, E),

    // ════════════════════════════════════════════════════════════
    // Section 6: Chorus 2 — raised intensity
    // ════════════════════════════════════════════════════════════
    Note::new(E5, E), Note::new(D5, E), Note::new(C5, E),
    Note::new(B4, Q), Note::new(A4, E), Note::new(B4, E),
    Note::new(C5, E), Note::new(D5, Q), Note::new(R, S),

    Note::new(E5, E), Note::new(D5, E), Note::new(C5, E),
    Note::new(B4, Q), Note::new(A4, E), Note::new(B4, E),
    Note::new(C5, E), Note::new(D5, Q), Note::new(R, S),

    Note::new(G5, E), Note::new(F5, E), Note::new(E5, E),
    Note::new(D5, Q), Note::new(C5, E), Note::new(B4, E),
    Note::new(C5, H), Note::new(R, Q),

    // ════════════════════════════════════════════════════════════
    // Section 7: Bridge / Interlude
    // ════════════════════════════════════════════════════════════
    Note::new(G5, E), Note::new(A5, E), Note::new(G5, Q),
    Note::new(F5, E), Note::new(E5, Q), Note::new(D5, E),
    Note::new(C5, Q), Note::new(R, S),

    Note::new(G5, E), Note::new(A5, E), Note::new(G5, Q),
    Note::new(F5, E), Note::new(E5, E), Note::new(D5, E),
    Note::new(E5, H), Note::new(R, E),

    Note::new(D5, E), Note::new(C5, E), Note::new(B4, E),
    Note::new(A4, Q), Note::new(G4, Q), Note::new(F4, Q),
    Note::new(E4, H), Note::new(R, E),

    // ════════════════════════════════════════════════════════════
    // Section 8: Final Chorus
    // ════════════════════════════════════════════════════════════
    Note::new(E5, S), Note::new(D5, S), Note::new(C5, S),
    Note::new(D5, Q), Note::new(C5, S), Note::new(B4, S),
    Note::new(A4, Q), Note::new(G4, E), Note::new(A4, E),
    Note::new(B4, S), Note::new(C5, Q), Note::new(R, S),

    Note::new(E5, S), Note::new(D5, S), Note::new(C5, S),
    Note::new(D5, Q), Note::new(C5, S), Note::new(B4, S),
    Note::new(A4, Q), Note::new(G4, E), Note::new(A4, E),
    Note::new(B4, S), Note::new(G4, H), Note::new(R, E),

    Note::new(G5, E), Note::new(F5, E), Note::new(E5, E),
    Note::new(D5, Q), Note::new(C5, E), Note::new(B4, E),
    Note::new(A4, Q), Note::new(G4, E), Note::new(F4, E),
    Note::new(G4, H), Note::new(R, Q),

    // ════════════════════════════════════════════════════════════
    // Section 9: Outro — winding down
    // ════════════════════════════════════════════════════════════
    Note::new(C5, Q), Note::new(B4, E), Note::new(A4, Q),
    Note::new(G4, E), Note::new(F4, Q), Note::new(D4, Q),
    Note::new(R, S),

    Note::new(C4, Q), Note::new(D4, Q), Note::new(E4, Q),
    Note::new(F4, Q), Note::new(G4, H), Note::new(A4, H),
    Note::new(R, W),

    // ── Final cadence ──────────────────────────────────────────
    Note::new(C5, H), Note::new(R, Q),
    Note::new(C5, E), Note::new(B4, E), Note::new(A4, H),
    Note::new(R, W),
];

/// Compute the total duration of the melody in milliseconds.
const fn total_duration_ms() -> u32 {
    let mut total = 0u32;
    let mut i = 0;
    while i < MELODY.len() {
        total += MELODY[i].duration_ms;
        i += 1;
    }
    total
}

// ── Section mapping ─────────────────────────────────────────────

/// Map a note index to a song section for animation variation.
const fn get_section(note_idx: usize) -> u32 {
    match note_idx {
        0..=31 => 0,    // Intro / Verse 1
        32..=49 => 1,   // Pre-Chorus 1
        50..=81 => 2,   // Chorus 1
        82..=113 => 3,  // Verse 2
        114..=131 => 4, // Pre-Chorus 2
        132..=158 => 5, // Chorus 2 (raised)
        159..=181 => 6, // Bridge
        182..=210 => 7, // Final Chorus
        _ => 8,         // Outro
    }
}

// ═══════════════════════════════════════════════════════════════
// Spin-loop calibration
// ═══════════════════════════════════════════════════════════════

/// Calibrate the busy-wait spin loop against the APIC timer tick counter.
///
/// Returns the number of `spin_loop()` iterations equivalent to 1 ms
/// on the current hardware.  Falls back to a conservative default if
/// the tick counter is not advancing.
fn calibrate_spin_loop() -> u64 {
    const TEST_SPINS: u64 = 3_000_000; // ~2 s on typical hardware
    const DEFAULT_ITERS_PER_MS: u64 = 1500;
    const TICK_DURATION_MS: u64 = 160; // APIC div=16, init=1M, bus≈100 MHz

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
    TEST_SPINS * 1000 / ms_elapsed.max(1)
}

// ═══════════════════════════════════════════════════════════════
// Fixed-point trigonometry
// ═══════════════════════════════════════════════════════════════

/// 12.4 fixed-point π constant (≈ 3.1416 × 16 = 50.265 → 50).
const PI_FP: i32 = 50;
/// 12.4 fixed-point 2π.
const TWO_PI_FP: i32 = 100;

/// Approximate sine using Bhaskara I's formula in integer arithmetic.
///
/// `x` is the phase in 12.4 fixed-point (0 … 100 = 0 … 2π).
/// Returns sin(x) as 12‑bit signed fixed‑point (−4096 … +4096).
#[inline]
fn approx_sin(x: i32) -> i32 {
    let mut x = x % TWO_PI_FP;
    if x > PI_FP {
        x -= TWO_PI_FP;
    } else if x < -PI_FP {
        x += TWO_PI_FP;
    }

    let sign = if x < 0 { -1i32 } else { 1i32 };
    let x_abs = x.abs();
    let pi_minus_x = PI_FP - x_abs;
    let numerator = 16 * x_abs * pi_minus_x;
    let denom = 12500 - 4 * x_abs * pi_minus_x;
    if denom == 0 {
        return 0;
    }
    sign * (numerator * 4096) / denom
}

/// Map an arbitrary `phase` counter to a sine value (−4096..+4096)
/// with cycle length `period`.
#[inline]
fn spi_sin(phase: u32, period: u32) -> i32 {
    if period == 0 {
        return 0;
    }
    approx_sin(((phase % period) as i32 * 100) / period as i32)
}

// ═══════════════════════════════════════════════════════════════
// Figure definition — articulated human silhouette
// ═══════════════════════════════════════════════════════════════

/// A body part: a circle at (cx, cy) with given radius.
///
/// All coordinates are in figure-local space — origin at the
/// figure centre, y-up positive, in unscaled pixel units.
#[derive(Clone, Copy)]
struct BodyCircle {
    cx: i32,
    cy: i32,
    radius: i32,
}

/// Compute the set of body-part circles and the four arm segments
/// for the current animation frame.
///
/// Returns:
/// - `circles`: head, neck, torso, shoulder joints, elbow joints, hand joints
/// - `arm_segments`: (shoulder→elbow, elbow→hand) × 2 arms, as ((x1,y1),(x2,y2))
/// - `arm_thickness`: pixel thickness of arm line segments
fn compute_figure(anim_t: u32, section: u32) -> (heapless::Vec<BodyCircle, 24>, heapless::Vec<((i32, i32), (i32, i32)), 4>, i32) {
    let mut circles: heapless::Vec<BodyCircle, 24> = heapless::Vec::new();
    let mut arms: heapless::Vec<((i32, i32), (i32, i32)), 4> = heapless::Vec::new();

    // ── Amplitude varies by section ───────────────────────
    let (arm_swing, body_sway) = match section {
        0 | 3 => (3000i32, 800i32),   // Verse: gentle
        1 | 4 => (5500, 1500),        // Pre-chorus: building
        2 | 5 | 7 => (7500, 2000),    // Chorus: energetic
        6 => (2000, 600),             // Bridge: subdued
        _ => (4000, 1000),            // Outro: winding down
    };

    // ── Body sway ─────────────────────────────────────────
    let sway = spi_sin(anim_t / 10, 600) * body_sway / 4096;

    // ── Head ──────────────────────────────────────────────
    let head_y = 70 + sway / 4;
    let head_r = 17;
    circles.push(BodyCircle { cx: 0, cy: head_y, radius: head_r }).ok();

    // ── Neck ──────────────────────────────────────────────
    circles.push(BodyCircle { cx: 0, cy: 52, radius: 6 }).ok();

    // ── Torso (three overlapping circles) ─────────────────
    circles.push(BodyCircle { cx: sway / 3, cy: 35, radius: 20 }).ok();
    circles.push(BodyCircle { cx: sway / 4, cy: 10, radius: 16 }).ok();
    circles.push(BodyCircle { cx: sway / 5, cy: -15, radius: 14 }).ok();

    // ── Shoulder joints ───────────────────────────────────
    let shoulder_l_x = -22 + sway / 3;
    let shoulder_r_x = 22 + sway / 3;
    let shoulder_y = 40;
    circles.push(BodyCircle { cx: shoulder_l_x, cy: shoulder_y, radius: 8 }).ok();
    circles.push(BodyCircle { cx: shoulder_r_x, cy: shoulder_y, radius: 8 }).ok();

    // ── Left arm ──────────────────────────────────────────
    let la_upper = spi_sin(anim_t / 6, 380) * arm_swing / 4096;
    let la_lower = spi_sin(anim_t / 5, 340) * arm_swing / 4096;

    let lelx = shoulder_l_x + (25 + la_upper / 256);
    let lely = shoulder_y + (-10 + la_upper / 200);
    let lhdx = lelx + (20 + la_lower / 300);
    let lhdy = lely + (-20 + la_lower / 200);

    circles.push(BodyCircle { cx: lelx, cy: lely, radius: 6 }).ok();
    circles.push(BodyCircle { cx: lhdx, cy: lhdy, radius: 5 }).ok();
    arms.push(((shoulder_l_x, shoulder_y), (lelx, lely))).ok();
    arms.push(((lelx, lely), (lhdx, lhdy))).ok();

    // ── Right arm ─────────────────────────────────────────
    let ra_upper = spi_sin(anim_t / 7, 410) * arm_swing / 4096;
    let ra_lower = spi_sin(anim_t / 5, 370) * arm_swing / 4096;

    let relx = shoulder_r_x + (-25 + ra_upper / 256);
    let rely = shoulder_y + (-10 - ra_upper / 200);
    let rhdx = relx + (-20 + ra_lower / 300);
    let rhdy = rely + (-20 - ra_lower / 200);

    circles.push(BodyCircle { cx: relx, cy: rely, radius: 6 }).ok();
    circles.push(BodyCircle { cx: rhdx, cy: rhdy, radius: 5 }).ok();
    arms.push(((shoulder_r_x, shoulder_y), (relx, rely))).ok();
    arms.push(((relx, rely), (rhdx, rhdy))).ok();

    (circles, arms, 6)
}

/// Test whether a pixel (in figure-local coordinates) lies inside
/// the silhouette defined by the body circles and arm segments.
#[inline]
fn pixel_in_figure(
    px: i32,
    py: i32,
    circles: &[BodyCircle],
    arm_segments: &[((i32, i32), (i32, i32))],
    arm_thickness: i32,
) -> bool {
    // Check body-part circles
    for c in circles {
        let dx = px - c.cx;
        let dy = py - c.cy;
        if dx * dx + dy * dy < c.radius * c.radius {
            return true;
        }
    }

    // Check arm line segments
    let thick_sq = arm_thickness * arm_thickness;
    for &((x1, y1), (x2, y2)) in arm_segments {
        let seg_dx = x2 - x1;
        let seg_dy = y2 - y1;
        let seg_len_sq = seg_dx * seg_dx + seg_dy * seg_dy;
        if seg_len_sq == 0 {
            continue;
        }
        let dot = (px - x1) * seg_dx + (py - y1) * seg_dy;
        // Clamp t to [0, seg_len_sq] using integer arithmetic
        let t = if dot <= 0 {
            0
        } else if dot >= seg_len_sq {
            seg_len_sq
        } else {
            dot
        };
        let proj_x = x1 * seg_len_sq + seg_dx * t;
        let proj_y = y1 * seg_len_sq + seg_dy * t;
        let ddx = px * seg_len_sq - proj_x;
        let ddy = py * seg_len_sq - proj_y;
        if ddx * ddx + ddy * ddy < thick_sq * seg_len_sq * seg_len_sq {
            return true;
        }
    }

    false
}

// ═══════════════════════════════════════════════════════════════
// Frame rendering
// ═══════════════════════════════════════════════════════════════

/// Draw a single animation frame onto the framebuffer.
///
/// Renders:
/// 1. A concentric-wave background (expanding ripples from centre).
/// 2. An articulated human silhouette (shadow-puppet effect)
///    by inverting pixels inside the figure.
fn draw_frame(
    fb: &mut [u32],
    fb_width: usize,
    fb_height: usize,
    anim_t: u32,
    section: u32,
) {
    let cx = (fb_width / 2) as i32;
    let cy = (fb_height / 2) as i32;

    // ── Build figure geometry ─────────────────────────────
    let (circles, arm_segments, arm_thickness) = compute_figure(anim_t, section);

    // ── Determine figure bounding box for early skip ──────
    let fig_half_w: i32 = 60;
    let fig_half_h: i32 = 100;
    let fbx0 = (cx - fig_half_w).max(0) as usize;
    let fbx1 = (cx + fig_half_w).min(fb_width as i32 - 1).max(0) as usize;
    let fby0 = (cy - fig_half_h).max(0) as usize;
    let fby1 = (cy + fig_half_h).min(fb_height as i32 - 1).max(0) as usize;

    let ripple_phase = (anim_t / 8) as i32;

    for y in 0..fb_height {
        let row_off = y * fb_width;
        let dy = y as i32 - cy;
        let in_fig_y = y >= fby0 && y <= fby1;

        for x in 0..fb_width {
            let idx = row_off + x;
            let dx = x as i32 - cx;

            // ── Concentric wave background ────────────────
            let dist = ((dx * dx + dy * dy) as u32 / 600) as i32;
            let wave = spi_sin((dist + ripple_phase) as u32, 80);
            let bg: u32 = if wave > 0 { 0xFFFFFFFF } else { 0xFF000000 };
            fb[idx] = bg;

            // ── Figure silhouette (invert pixels inside) ──
            if in_fig_y && x >= fbx0 && x <= fbx1 {
                let fx = x as i32 - cx;
                let fy = y as i32 - cy;
                if pixel_in_figure(fx, fy, &circles, &arm_segments, arm_thickness) {
                    fb[idx] = !fb[idx];
                }
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Main playback
// ═══════════════════════════════════════════════════════════════

/// Play the full Bad Apple melody on PC speaker while animating
/// the framebuffer with a shadow-puppet effect.
///
/// # How it works
///
/// 1. Calibrates a busy-wait spin loop against the APIC tick counter.
/// 2. Fills the screen with black, then enters the playback loop.
/// 3. Each iteration advances note playback, updates the PC speaker,
///    and renders one animation frame.
/// 4. Frame delay uses the calibrated spin-loop for consistent fps.
/// 5. On exit (end of melody or user abort), restores the desktop.
pub fn play_badapple() {
    log::info!("Bad Apple playback started");

    // ── Get framebuffer ────────────────────────────────────
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

    let song_length_ms = total_duration_ms();
    log::info!(
        "Bad Apple: {}x{} framebuffer, {} notes, {:.1} s melody",
        fb_width,
        fb_height,
        MELODY.len(),
        song_length_ms as f64 / 1000.0
    );

    // ── Timing calibration ─────────────────────────────────
    let iters_per_ms = calibrate_spin_loop();
    const TARGET_FPS: u64 = 12;
    const FRAME_MS: u64 = 1000 / TARGET_FPS; // ~83 ms → 12 fps
    let frame_spins = FRAME_MS * iters_per_ms;

    log::info!(
        "Bad Apple: {} iters/ms, target {} fps ({} spins/frame)",
        iters_per_ms,
        TARGET_FPS,
        frame_spins
    );

    // ── Playback state ────────────────────────────────────
    let mut elapsed_ms: u64 = 0;
    let mut cursor: usize = 0;
    let mut note_start_ms: u64 = 0;
    // Start the first note
    crate::sound::pc_speaker_on(MELODY[0].freq_hz);
    let mut prev_freq: u32 = MELODY[0].freq_hz;

    // ── Prepare framebuffer ────────────────────────────────
    let fb = unsafe { core::slice::from_raw_parts_mut(fb_ptr, fb_len) };
    // Fill with black to start clean
    for pixel in fb.iter_mut() {
        *pixel = 0xFF000000;
    }

    // ── Main playback / animation loop ────────────────────
    while elapsed_ms < song_length_ms as u64 {
        // ── Abort on keypress ─────────────────────────────
        if nitrogen::ps2::keyboard::input_available() {
            log::info!("Bad Apple aborted by user");
            while nitrogen::ps2::keyboard::read_char().is_some() {}
            break;
        }

        // ── Advance note cursor if current note expired ───
        let mut note_changed = false;
        loop {
            if cursor >= MELODY.len() {
                break;
            }
            let cur_dur = MELODY[cursor].duration_ms as u64;
            if elapsed_ms < note_start_ms + cur_dur {
                break;
            }
            note_start_ms += cur_dur;
            cursor += 1;
            note_changed = true;
        }

        // ── Update speaker (only on note change) ──────────
        if cursor >= MELODY.len() {
            if prev_freq != 0 {
                crate::sound::pc_speaker_off();
                prev_freq = 0;
            }
        } else if note_changed {
            let freq = MELODY[cursor].freq_hz;
            if freq != prev_freq {
                if freq == 0 {
                    crate::sound::pc_speaker_off();
                } else {
                    crate::sound::pc_speaker_on(freq);
                }
                prev_freq = freq;
            }
        }

        // ── Determine current section for animation ───────
        let section = get_section(cursor.min(MELODY.len().saturating_sub(1)));

        // ── Draw animation frame ──────────────────────────
        draw_frame(fb, fb_width, fb_height, elapsed_ms as u32, section);

        // ── Frame delay (calibrated busy-wait) ────────────
        for _ in 0..frame_spins {
            core::hint::spin_loop();
        }

        elapsed_ms += FRAME_MS;
    }

    // ── Cleanup ───────────────────────────────────────────
    crate::sound::pc_speaker_off();
    solvent::force_desktop_redraw();

    log::info!(
        "Bad Apple playback finished ({} s elapsed)",
        elapsed_ms as f64 / 1000.0
    );
}