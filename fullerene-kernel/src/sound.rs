//! Sound / Audio subsystem for Fullerene OS.
//!
//! ## Architecture
//!
//! - **PC Speaker** — simple PIT‑driven square‑wave beeper.
//! - **HDA** — Intel High Definition Audio.
//!
//! All state lives in [`crate::contexts::AudioContext`].  This module
//! provides thin wrapper functions so existing callers (`badapple.rs`,
//! `shell.rs`, `graphics/mod.rs`) continue to compile without changes.

use crate::contexts::audio::{with_audio, with_audio_mut, AudioContext};

// ── PC Speaker ───────────────────────────────────────────────────

pub fn pc_speaker_on(frequency_hz: u32) {
    AudioContext::pc_speaker_on(frequency_hz);
}

pub fn pc_speaker_off() {
    AudioContext::pc_speaker_off();
}

// ── HDA wrappers ──────────────────────────────────────────────────

/// Re-export diagnostic info for shell inspection.
pub use nitrogen::hda::controller::HdaDiagInfo;

/// Get a snapshot of HDA diagnostic info.
pub fn hda_diag() -> HdaDiagInfo {
    with_audio(|ctx| ctx.diag).unwrap_or(HdaDiagInfo {
        gcap: 0,
        gcap64: false,
        corb_phys: 0,
        rirb_phys: 0,
        states_after_crst: 0,
        populated: false,
    })
}

pub fn init() {
    // AudioContext is initialised in the contexts init step.
    // This is a no-op for backwards compatibility.
}

pub fn hda_available() -> bool {
    with_audio(|ctx| ctx.hda_available()).unwrap_or(false)
}

pub fn hda_tick() {
    nitrogen::hda::HdaController::tick_vm_exit();
}

pub fn hda_write_direct(offset: u32, samples: &[u8]) -> usize {
    with_audio_mut(|ctx| ctx.write_samples(offset, samples)).unwrap_or(0)
}

pub fn hda_reset_prefill_tracking() {
    if let Some(()) = with_audio_mut(|ctx| {
        ctx.reset_prefill_tracking();
    }) {}
}

pub fn hda_feed_samples(samples: &[u8]) -> usize {
    with_audio_mut(|ctx| ctx.feed_samples(samples)).unwrap_or(0)
}

#[inline]
pub fn hda_feed_pcm(pcm: &[u8], pcm_off: &mut usize, pcm_total: usize, half: usize) -> usize {
    let off = *pcm_off;
    if off >= pcm_total {
        return 0;
    }
    let rem = pcm_total - off;
    let end = (off + rem.min(half)).min(pcm_total);
    let fed = hda_feed_samples(&pcm[off..end]);
    if fed > 0 {
        *pcm_off += fed;
    }
    fed
}

pub fn hda_poll() {
    if let Some(()) = with_audio_mut(|ctx| {
        ctx.poll();
    }) {}
}

pub fn hda_poll_block(timeout_tsc: Option<u64>) -> bool {
    with_audio(|ctx| ctx.poll_block(timeout_tsc)).unwrap_or(false)
}

pub fn hda_poll_delay(tsc_per_ms: u64, ms: u64) {
    if let Some(()) = with_audio_mut(|ctx| {
        ctx.poll_delay(tsc_per_ms, ms);
    }) {}
}

pub fn hda_playback_progress() -> Option<u64> {
    with_audio(|ctx| ctx.playback_progress()).flatten()
}

pub fn hda_feed_silence(half: usize) -> usize {
    with_audio_mut(|ctx| ctx.feed_silence(half)).unwrap_or(0)
}