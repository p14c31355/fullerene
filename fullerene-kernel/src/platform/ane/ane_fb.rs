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

/// The full-screen canvas: the DT's `graphic` reserved region, 0x31000000,
/// 0x1A40000 (26.2 MiB). The display controller scans this region out when
/// it has no other buffer, so painting here is visible directly *and* it is
/// what covers the loader's unlock warning on the panel.
const CANVAS: usize = 0x3100_0000;
const WIDTH: u32 = 1080;
const HEIGHT: u32 = 2340;
/// 1080 x 2340 x 4 = 9.64 MiB - well inside the 26.2 MiB reserve.
const CANVAS_LEN: usize = (WIDTH as usize) * (HEIGHT as usize) * 4;

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
    // Never copy the canvas onto itself (overlapping copy_nonoverlapping is
    // UB, and one window register can hold exactly the canvas address).
    if overlaps(CANVAS as u64, CANVAS as u64 + CANVAS_LEN as u64) {
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
        // Clear the cell band first: the glyph renderer draws ink only (it
        // never erases), so repainting a row over its previous content left
        // old digits showing through as mojibake in the strip.
        let x0 = TEXT_X as usize;
        let x1 = (TEXT_X + LINE_CHARS as u32 * 8) as usize;
        for row in y..(y + GLYPH_H).min(HEIGHT) {
            let base = row as usize * WIDTH as usize;
            for px in &mut canvas[base + x0..base + x1] {
                *px = BG;
            }
        }
        lattice::font::render_text(
            canvas, WIDTH, HEIGHT, WIDTH, TEXT_X, y, &self.buf, FG, GLYPH_H,
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

    // The canvas IS the scanned-out region (the DT's `graphic` reserve), so
    // painting it was already the visible step - and it is what covers the
    // loader's unlock warning. The copies to accepted window targets remain
    // for the layers the sweep finds. Make the canvas durable too: with the
    // data cache on the panel would otherwise scan stale pixels.
    for &addr in &targets[..ok] {
        unsafe {
            ptr::copy_nonoverlapping(CANVAS as *const u8, addr as *mut u8, CANVAS_LEN);
            crate::blackbox::cache_clean(addr as usize, CANVAS_LEN);
        }
    }
    log("fb: blits=", ok as u64);
    unsafe {
        crate::blackbox::cache_clean(CANVAS, CANVAS_LEN);
    }
    log("fb: flushed=", 1);
}

// ---- the fastboot live diagnostic strip -------------------------------------

/// Where the strip starts (below the boot report's block).
const DIAG_Y: u32 = TEXT_Y + 14 * LINE_PITCH;
/// The strip is repainted only when the state fold changes.
static mut DIAG_FOLD: u64 = 0;
/// Strips rows to refresh (the line count plus a margin).
const DIAG_ROWS: u32 = 9 * LINE_PITCH;

/// Paint a bring-up data line below the diagnostic strip (rows 0..).
///
/// The panel is the fastest, most reliable channel on this board (USB and
/// the durable log both need working machinery; a photo of the screen needs
/// nothing). Callers paint one line per measured value; a hung boot leaves
/// the last completed line visible for the photo.
pub fn paint(row: u32, text: &[u8]) {
    let canvas: &mut [u32] =
        unsafe { core::slice::from_raw_parts_mut(CANVAS as *mut u32, CANVAS_LEN / 4) };
    let y = DIAG_Y + (10 + row) * LINE_PITCH;
    let mut buf = [b' '; LINE_CHARS];
    let n = text.len().min(LINE_CHARS);
    buf[..n].copy_from_slice(&text[..n]);
    Line {
        buf,
        at: LINE_CHARS,
    }
    .draw(canvas, y);
    unsafe {
        crate::blackbox::cache_clean(CANVAS, CANVAS_LEN);
    }
}

/// Copy the whole canvas to every accepted scan-out window (the same blit
/// `report` performs at boot). Bring-up code that paints after the report
/// must call this once: the panel can be scanning a window target, and a
/// canvas-only edit never reaches it.
pub fn flush_panel() {
    for &ch in CHANNELS.iter() {
        for &w in WINDOWS.iter() {
            let addr = unsafe { ptr::read_volatile((ch + w) as *const u32) };
            if blit_ok(addr) {
                unsafe {
                    ptr::copy_nonoverlapping(CANVAS as *const u8, addr as *mut u8, CANVAS_LEN);
                    crate::blackbox::cache_clean(addr as usize, CANVAS_LEN);
                }
            }
        }
    }
    unsafe {
        crate::blackbox::cache_clean(CANVAS, CANVAS_LEN);
    }
}

/// Repaint the fastboot diagnostic strip when the responder's state changed
/// since the last paint. Called from the wait loop, so a photo of the panel
/// shows the last exchange: FIFO layout, counters, registers.
pub fn draw_fb_diag(d: &super::ane_usb_dev::FbDiag) {
    let mut fold = 1u64;
    for v in [
        d.dptxfsiz1,
        d.dtxfsts1,
        d.gnptxfsiz,
        d.grxfsiz,
        d.tx_idx,
        d.lk_diepctl1,
        d.lk_doepctl1,
        d.diepctl1,
        d.doepctl1,
        d.fb_len,
        d.fb_disp,
        d.fb_reply,
        d.rx_entries,
        d.ep1_bytes,
        d.last_rxsts,
        d.gintsts,
        d.epint,
    ] {
        fold = fold.wrapping_mul(31).wrapping_add(v as u64);
    }
    fold = fold.wrapping_mul(31).wrapping_add(d.cmd_head);
    if fold == unsafe { core::ptr::addr_of!(DIAG_FOLD).read() } {
        return;
    }
    unsafe {
        core::ptr::addr_of_mut!(DIAG_FOLD).write(fold);
    }

    let canvas: &mut [u32] =
        unsafe { core::slice::from_raw_parts_mut(CANVAS as *mut u32, CANVAS_LEN / 4) };
    let mut y = DIAG_Y;
    Line::new()
        .s(b"FB0 DPTX1=")
        .x(d.dptxfsiz1)
        .s(b" DTXF1=")
        .x(d.dtxfsts1)
        .draw(canvas, y);
    y += LINE_PITCH;
    Line::new()
        .s(b"FB1 GRX=")
        .x(d.grxfsiz)
        .s(b" TXIDX=")
        .x(d.tx_idx)
        .draw(canvas, y);
    y += LINE_PITCH;
    Line::new()
        .s(b"FB2 LKDIE=")
        .x(d.lk_diepctl1)
        .s(b" LKDOE=")
        .x(d.lk_doepctl1)
        .draw(canvas, y);
    y += LINE_PITCH;
    Line::new()
        .s(b"FB3 DIEP=")
        .x(d.diepctl1)
        .s(b" DOEP=")
        .x(d.doepctl1)
        .draw(canvas, y);
    y += LINE_PITCH;
    Line::new()
        .s(b"FB4 LEN=")
        .x(d.fb_len)
        .s(b" DSP=")
        .x(d.fb_disp)
        .s(b" RPLY=")
        .x(d.fb_reply)
        .draw(canvas, y);
    y += LINE_PITCH;
    Line::new()
        .s(b"FB5 RXE=")
        .x(d.rx_entries)
        .s(b" RXD=")
        .x(d.ep1_bytes)
        .s(b" GSTS=")
        .x(d.gintsts)
        .draw(canvas, y);
    y += LINE_PITCH;
    Line::new()
        .s(b"FB6 LAST=")
        .x(d.last_rxsts)
        .s(b" EPINT=")
        .x(d.epint)
        .draw(canvas, y);
    y += LINE_PITCH;
    Line::new()
        .s(b"FB7 CMD=")
        .x((d.cmd_head >> 32) as u32)
        .x(d.cmd_head as u32)
        .draw(canvas, y);

    // Refresh the strip on every accepted scan-out window and make the
    // canvas strip itself durable (the data cache is on).
    let off = DIAG_Y as usize * WIDTH as usize * 4;
    let bytes = DIAG_ROWS as usize * WIDTH as usize * 4;
    for &ch in CHANNELS.iter() {
        for &w in WINDOWS.iter() {
            let addr = unsafe { ptr::read_volatile((ch + w) as *const u32) };
            if blit_ok(addr) {
                unsafe {
                    ptr::copy_nonoverlapping(
                        (CANVAS + off) as *const u8,
                        (addr as usize + off) as *mut u8,
                        bytes,
                    );
                    crate::blackbox::cache_clean(addr as usize + off, bytes);
                }
            }
        }
    }
    unsafe {
        crate::blackbox::cache_clean(CANVAS + off, bytes);
    }
}
