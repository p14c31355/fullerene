//! The terminal-generic ring diagnostics (the v331 refactor).
//!
//! Every bring-up sequence logs numbered stage markers to the durable
//! ring (the T1..T8 pattern that located the touch probe's parking
//! write). This module generalizes that pattern:
//!
//! - [`Stage`]: the named boot stages (replaces the scattered b'H'/b'R'
//!   stage bytes).
//! - [`ring!`]/[`ring_val!`]: the numbered marker macros. The stop
//!   position of a wedged boot is the last logged marker - the step
//!   that follows it is the suspect.
//!
//! The backend is the platform ring writer (the ANE's
//! `ane_fb::event`/`blackbox::text_puts`); the macros only format.

/// The named boot stages. The single-byte encodings match the
/// STG_STAGE values already on the wire (the IQ line's G= byte).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The very early platform init (the clock gates, the MMU).
    Init,
    /// The first-light panel paint (before any USB work).
    FirstLight,
    /// The mobile home screen rendered.
    Home,
    /// The USB enumeration / reenumerate window.
    Reenum,
    /// The solvent desktop render (the composite proof).
    Desktop,
    /// The touch bring-up sequence.
    Touch,
    /// The storage (eMMC/UFS) bring-up.
    Storage,
}

impl Stage {
    /// The single-byte stage code the ring lines carry.
    pub const fn code(self) -> u8 {
        match self {
            Stage::Init => b'I',
            Stage::FirstLight => b'F',
            Stage::Home => b'H',
            Stage::Reenum => b'R',
            Stage::Desktop => b'D',
            Stage::Touch => b'T',
            Stage::Storage => b'S',
        }
    }
}

/// The ring writer contract. The platform implements this once; the
/// macros call it. (The ANE backend tees into the durable ring via
/// blackbox::text_puts.)
pub trait RingWriter {
    fn write_bytes(&self, bytes: &[u8]);
}

/// The numbered stage marker: composes the "T3: init" style lines (the
/// T1..T8 pattern that located the touch probe's parking write). The
/// platform passes its stage sequence number and the step text; the
/// writer carries it into the durable ring.
pub fn stage_marker(writer: &dyn RingWriter, stage: Stage, step: u8, text: &[u8]) {
    // The line: <stage code><step>: <text>\n
    let mut line = [0u8; 4 + 64];
    line[0] = stage.code();
    let n = (step / 10) as usize;
    line[1] = b'0' + (step % 10);
    line[2] = b':';
    line[3] = b' ';
    let text_len = text.len().min(60);
    line[4..4 + text_len].copy_from_slice(&text[..text_len]);
    let total = 4 + text_len;
    line[total] = b'\n';
    writer.write_bytes(&line[..total + 1]);
}
