//! The ANE's on-screen self-report, driven from Rust.
//!
//! Why this module exists: the USB bring-up runs in the same boot as this
//! renderer - cross-boot handoff is impossible (the loader re-initialises
//! DRAM every boot) and the loader's own consoles are dead on this handset
//! (PL011 unconnected, no working pstore console). The .S probe family
//! proved the display path: paint a canvas in the loader's display reserve,
//! then copy it to every candidate layer the display controller scans out.
//! This module ports exactly that blit, but paints through `lattice::font`
//! so the boot that runs the bring-up prints the whole report itself.
//!
//! Measured layout facts (kept verbatim from the probe family):
//!   - canvas: the loader's display reserve, 0x210000 bytes, 1080 px rows;
//!   - each display channel exposes three window registers at +0x60/+0x84/
//!     +0xA8 holding the DMA data address of one scanned-out window;
//!   - gates: skip addresses under 1 MiB, at or above 0xC0000000, or whose
//!     window would cover the display reserve or the durable-log region;
//!   - pixel format: u32 ARGB, the probe's measured text colour is yellow.

use core::ptr;

use super::ane::{self, UsbState};

/// The loader's display reserve - the probe family's proven canvas.
const CANVAS: usize = 0x01AE_3000;
/// Canvas byte length (500 rows of 1080 px at 4 bytes each).
const CANVAS_LEN: usize = 0x21_0000;
/// The probe family's fixed fallback blit target (reserved graphic region).
const FALLBACK: usize = 0x3100_0000;
const WIDTH: u32 = 1080;
const HEIGHT: u32 = (CANVAS_LEN / (WIDTH as usize * 4)) as u32;

/// Opaque yellow - the probe family's measured text colour.
const FG: u32 = 0xFFFF_FF00;
/// Opaque black.
const BG: u32 = 0xFF00_0000;

const TEXT_X: u32 = 8;
const TEXT_Y: u32 = 110;
const LINE_PITCH: u32 = 24;
const GLYPH_H: u32 = 13;
/// 44 columns at 8 px per glyph stay clear of the 1080 px row.
const LINE_CHARS: usize = 44;

/// Layered display channel bases: VG0, VG1, G0, G1, D0..D3, WCH0, WCH1.
const CHANNELS: [usize; 10] = [
    0xE862_0000,
    0xE862_8000,
    0xE863_8000,
    0xE864_0000,
    0xE865_0000,
    0xE865_1000,
    0xE865_2000,
    0xE865_3000,
    0xE865_A000,
    0xE865_C000,
];

/// Per-channel window registers holding a scanned-out DMA data address.
const WINDOWS: [usize; 3] = [0x60, 0x84, 0xA8];

const HEX: [u8; 16] = *b"0123456789ABCDEF";

/// Range gates copied from the probe blit: never write outside plain DRAM,
/// never onto the display reserve itself, never onto the durable-log window.
fn blit_ok(addr: u32) -> bool {
    if addr < 0x0010_0000 {
        return false;
    }
    if addr >= 0xC000_0000 {
        return false;
    }
    let lo = addr as u64;
    let hi = lo + CANVAS_LEN as u64;
    let overlaps = |wlo: u64, whi: u64| lo < whi && hi > wlo;
    if overlaps(0x01AE_0000, 0x01B1_0000) {
        return false;
    }
    if overlaps(0x3480_0000, 0x3490_0000) {
        return false;
    }
    // Never write over this kernel's own image (loaded at 0x1660000, well
    // under 1.6 MiB) - a candidate pointing there once corrupted the boot's
    // own text/tee state.
    if overlaps(0x0160_0000, 0x0190_0000) {
        return false;
    }
    true
}

/// Write eight upper-case hex digits at `at`.
fn hex8(buf: &mut [u8], at: usize, v: u32) {
    for i in 0..8 {
        buf[at + i] = HEX[((v >> (28 - 4 * i)) & 0xF) as usize];
    }
}

/// Copy `s` into `buf` at `at`, returning the next write position.
fn put(buf: &mut [u8], at: usize, s: &[u8]) -> usize {
    let n = core::cmp::min(s.len(), buf.len() - at);
    buf[at..at + n].copy_from_slice(&s[..n]);
    at + n
}

/// One report line: a fixed 44-column buffer built label/value by label/value.
struct Line {
    buf: [u8; LINE_CHARS],
    at: usize,
}

impl Line {
    fn new() -> Self {
        Self {
            buf: [b' '; LINE_CHARS],
            at: 0,
        }
    }

    fn s(mut self, s: &[u8]) -> Self {
        self.at = put(&mut self.buf, self.at, s);
        self
    }

    fn x(mut self, v: u32) -> Self {
        if self.at + 8 <= LINE_CHARS {
            hex8(&mut self.buf, self.at, v);
        }
        self.at += 8;
        self
    }

    fn draw(&self, canvas: &mut [u32], y: u32) {
        lattice::font::render_text(
            canvas,
            WIDTH,
            HEIGHT,
            WIDTH,
            TEXT_X,
            y,
            &self.buf,
            FG,
            GLYPH_H,
        );
    }
}

/// Paint the boot report (USB state + watchdog snapshot + sweep stats) and
/// blit it to every candidate layer. Runs in the boot that performed the
/// bring-up: the writer is also the reporter. Every stage is also mirrored
/// to the console (uart -> durable log) so the v7f reader can show where a
/// silent stop happened even when the panel shows nothing.
pub fn report(usb: &UsbState, log: fn(&str, u64)) {
    let wd = ane::watchdog_state();

    log("fb: enter=", 1);

    // Dry-run sweep first: read every candidate and keep the addresses the
    // gates accept, so the stats can be painted before the copies happen.
    let mut targets = [0u32; 32];
    let mut ok = 0usize;
    let mut gated = 0usize;
    let mut probe = [0u32; 6];
    let mut pi = 0usize;
    for &ch in CHANNELS.iter() {
        for &w in WINDOWS.iter() {
            let addr = unsafe { ptr::read_volatile((ch + w) as *const u32) };
            if pi < probe.len() {
                probe[pi] = addr;
                pi += 1;
            }
            if blit_ok(addr) && ok < targets.len() {
                targets[ok] = addr;
                ok += 1;
            } else {
                gated += 1;
            }
        }
    }
    log("fb: ok=", ok as u64);
    log("fb: gated=", gated as u64);
    log("fb: c0=", probe[0] as u64);
    log("fb: c1=", probe[1] as u64);
    log("fb: c2=", probe[2] as u64);

    // The canvas lives in reserved DRAM this boot owns; wrap it as pixels.
    let canvas: &mut [u32] =
        unsafe { core::slice::from_raw_parts_mut(CANVAS as *mut u32, CANVAS_LEN / 4) };
    for px in canvas.iter_mut() {
        *px = BG;
    }
    log("fb: canvas=", 1);

    let mut y = TEXT_Y;
    Line::new().s(b"ANE SELF-REPORT (RUST FB)").draw(canvas, y);
    y += LINE_PITCH;
    Line::new().s(b"BOOT OK - USB BRING-UP RAN").draw(canvas, y);
    y += LINE_PITCH;
    Line::new()
        .s(b"ANE: SP805 LOAD=")
        .x(wd.load)
        .s(b" VALUE=")
        .x(wd.value)
        .draw(canvas, y);
    y += LINE_PITCH;
    Line::new()
        .s(b"ANE: SP805 CTRL=")
        .x(wd.control)
        .s(b" LOCK=")
        .x(wd.lock)
        .draw(canvas, y);
    y += LINE_PITCH;
    Line::new()
        .s(b"USB: ABB=")
        .x(usb.abb)
        .s(b" PPPLL0=")
        .x(usb.ppll0.0)
        .s(b"->")
        .x(usb.ppll0.1)
        .draw(canvas, y);
    y += LINE_PITCH;
    Line::new()
        .s(b"USB: GUSBCFG=")
        .x(usb.core[0])
        .s(b" DCFG=")
        .x(usb.core[1])
        .draw(canvas, y);
    y += LINE_PITCH;
    Line::new()
        .s(b"USB: DCTL=")
        .x(usb.core[2])
        .s(b" GINTSTS=")
        .x(usb.core[3])
        .draw(canvas, y);
    y += LINE_PITCH;
    Line::new()
        .s(b"USB: PHY0=")
        .x(usb.phy[0])
        .s(b" PHY4=")
        .x(usb.phy[1])
        .draw(canvas, y);
    y += LINE_PITCH;
    Line::new()
        .s(b"USB: PHY8=")
        .x(usb.phy[2])
        .s(b" PHYC=")
        .x(usb.phy[3])
        .draw(canvas, y);
    y += LINE_PITCH;
    Line::new()
        .s(b"SWEEP: OK=")
        .x(ok as u32)
        .s(b" GATED=")
        .x(gated as u32)
        .draw(canvas, y);
    y += LINE_PITCH;
    Line::new()
        .s(b"C0=")
        .x(probe[0])
        .s(b" C1=")
        .x(probe[1])
        .s(b" C2=")
        .x(probe[2])
        .draw(canvas, y);

    log("fb: paint=", 1);

    // Copy the canvas to every accepted target, then to the probe family's
    // fixed fallback (0x31000000, the reserved graphic region) so the stats
    // are visible even when the sweep accepts nothing. The flush makes the
    // pixels reach DRAM while the boot still runs.
    for &addr in &targets[..ok] {
        unsafe {
            ptr::copy_nonoverlapping(CANVAS as *const u8, addr as *mut u8, CANVAS_LEN);
            crate::blackbox::cache_clean(addr as usize, CANVAS_LEN);
        }
    }
    log("fb: blits=", ok as u64);
    unsafe {
        ptr::copy_nonoverlapping(CANVAS as *const u8, FALLBACK as *mut u8, CANVAS_LEN);
        crate::blackbox::cache_clean(FALLBACK, CANVAS_LEN);
    }
    log("fb: fallback=", 1);
}
