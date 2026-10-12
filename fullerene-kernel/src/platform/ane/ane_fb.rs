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
pub const CANVAS: usize = 0x3100_0000;
pub const WIDTH: u32 = 1080;
pub const HEIGHT: u32 = 2340;
/// 1080 x 2340 x 4 = 9.64 MiB - well inside the 26.2 MiB reserve.
const CANVAS_LEN: usize = (WIDTH as usize) * (HEIGHT as usize) * 4;

/// The boot console uses a restrained dark palette so diagnostic values stay
/// readable without competing with the desktop that follows it.
const BG: u32 = 0xFF07_1019;
const PANEL: u32 = 0xFF10_1D2A;
const PANEL_ALT: u32 = 0xFF14_2637;
const GRID: u32 = 0xFF1D_384A;
const FG: u32 = 0xFFE7_F2F5;
const MUTED: u32 = 0xFF86_A3AF;
const CYAN: u32 = 0xFF41_D7D0;
const GREEN: u32 = 0xFF57_D68D;
const AMBER: u32 = 0xFFFF_C857;

/// Set by the desktop bring-up once `solvent::render` owns the canvas.
///
/// The boot instruments below double as fastboot forensics; they stop
/// painting the moment the desktop takes the canvas so their text rows
/// cannot scribble over the desktop pixels. The durable ring and the
/// flush helpers are unaffected (the flushes copy whatever the canvas
/// already holds).
pub static DESKTOP_OWNED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// True while the desktop owns the canvas.
fn desktop_owned() -> bool {
    DESKTOP_OWNED.load(core::sync::atomic::Ordering::Relaxed)
}

/// Set by the desktop bring-up before it does any real work: the per-IRQ
/// instrument paints stop, so the timer interrupt no longer spends its
/// budget copying canvas bands (up to a full 9.6 MiB flush_panel) while
/// the desktop's own multi-MiB fill/render needs the CPU. The durable
/// ring, the restamps and `flush_panel` itself stay available.
pub static IRQ_QUIET: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// True while the desktop wants the IRQ paints quiet.
fn irq_quiet() -> bool {
    IRQ_QUIET.load(core::sync::atomic::Ordering::Relaxed)
}

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

/// Fill a clipped rectangle without allocating or depending on a compositor.
fn rect(canvas: &mut [u32], x: u32, y: u32, width: u32, height: u32, color: u32) {
    let x0 = x.min(WIDTH) as usize;
    let y0 = y.min(HEIGHT) as usize;
    let x1 = x.saturating_add(width).min(WIDTH) as usize;
    let y1 = y.saturating_add(height).min(HEIGHT) as usize;
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    for row in y0..y1 {
        let start = row * WIDTH as usize + x0;
        canvas[start..start + (x1 - x0)].fill(color);
    }
}

fn ui_text(canvas: &mut [u32], x: u32, y: u32, text: &[u8], color: u32) {
    lattice::font::render_text(canvas, WIDTH, HEIGHT, WIDTH, x, y, text, color, GLYPH_H);
}

fn ui_hex(canvas: &mut [u32], x: u32, y: u32, label: &[u8], value: u32, color: u32) {
    let mut buf = [b' '; 32];
    let mut at = 0usize;
    at = put(&mut buf, at, label);
    at = put(&mut buf, at, b" 0x");
    for shift in (0..8).rev() {
        buf[at] = HEX[((value >> (shift * 4)) & 0xF) as usize];
        at += 1;
    }
    ui_text(canvas, x, y, &buf[..at], color);
}

fn panel(canvas: &mut [u32], x: u32, y: u32, width: u32, height: u32, color: u32) {
    rect(canvas, x, y, width, height, color);
    rect(canvas, x, y, width, 1, GRID);
    rect(canvas, x, y + height.saturating_sub(1), width, 1, GRID);
    rect(canvas, x, y, 1, height, GRID);
    rect(canvas, x + width.saturating_sub(1), y, 1, height, GRID);
}

fn stage(canvas: &mut [u32], x: u32, y: u32, label: &[u8], done: bool) {
    panel(canvas, x, y, 168, 88, PANEL);
    let color = if done { GREEN } else { AMBER };
    rect(canvas, x + 16, y + 18, 18, 18, color);
    rect(
        canvas,
        x + 21,
        y + 23,
        8,
        8,
        if done { PANEL } else { PANEL_ALT },
    );
    ui_text(canvas, x + 48, y + 20, label, FG);
    ui_text(
        canvas,
        x + 48,
        y + 48,
        if done { b"READY" } else { b"PENDING" },
        color,
    );
}

fn draw_boot_screen(
    canvas: &mut [u32],
    usb: &UsbState,
    wd: ane::WatchdogState,
    ok: usize,
    gated: usize,
    probe: [u32; 6],
) {
    // Header: a block F mark remains recognisable even when the font is not
    // loaded yet.
    rect(canvas, 72, 72, 936, 4, CYAN);
    rect(canvas, 72, 104, 56, 56, CYAN);
    rect(canvas, 88, 120, 32, 8, BG);
    rect(canvas, 88, 136, 24, 8, BG);
    ui_text(canvas, 160, 102, b"FULLERENE OS", FG);
    ui_text(canvas, 160, 130, b"ANE / BOOT CONSOLE", MUTED);

    panel(canvas, 72, 220, 936, 176, PANEL);
    ui_text(canvas, 104, 248, b"BOOT CONSOLE", MUTED);
    ui_text(canvas, 104, 286, b"USB BRING-UP", FG);
    ui_text(
        canvas,
        104,
        326,
        b"DISPLAY PATH ONLINE / DIAGNOSTICS RETAINED",
        CYAN,
    );
    let status: &[u8] = if ok > 0 { b"ONLINE" } else { b"WAITING" };
    let status_color = if ok > 0 { GREEN } else { AMBER };
    ui_text(canvas, 824, 286, status, status_color);
    rect(canvas, 824, 330, 120, 10, GRID);
    rect(
        canvas,
        824,
        330,
        if ok > 0 { 120 } else { 34 },
        10,
        status_color,
    );

    ui_text(canvas, 72, 458, b"BOOT PIPELINE", MUTED);
    stage(canvas, 72, 492, b"CLOCK", true);
    stage(
        canvas,
        252,
        492,
        b"USB PHY",
        usb.phy.iter().any(|&v| v != 0),
    );
    stage(canvas, 432, 492, b"GADGET", usb.core[3] != 0);
    stage(canvas, 612, 492, b"FRAMEBUFFER", ok > 0);
    stage(canvas, 792, 492, b"LAUNCHD", false);

    panel(canvas, 72, 632, 936, 548, PANEL);
    ui_text(canvas, 104, 664, b"LIVE DIAGNOSTICS / USB + PHY", MUTED);
    ui_hex(canvas, 104, 716, b"ABB", usb.abb, FG);
    ui_hex(canvas, 560, 716, b"GUSBCFG", usb.core[0], FG);
    ui_hex(canvas, 104, 764, b"PPPLL0", usb.ppll0.0, FG);
    ui_hex(canvas, 560, 764, b"DCFG", usb.core[1], FG);
    ui_hex(canvas, 104, 812, b"DCTL", usb.core[2], FG);
    ui_hex(canvas, 560, 812, b"GINTSTS", usb.core[3], FG);
    ui_hex(canvas, 104, 860, b"PHY0", usb.phy[0], CYAN);
    ui_hex(canvas, 560, 860, b"PHY4", usb.phy[1], CYAN);
    ui_hex(canvas, 104, 908, b"PHY8", usb.phy[2], CYAN);
    ui_hex(canvas, 560, 908, b"PHYC", usb.phy[3], CYAN);
    rect(canvas, 104, 974, 872, 1, GRID);
    ui_text(canvas, 104, 1002, b"WINDOW SWEEP", MUTED);
    ui_hex(canvas, 104, 1048, b"ACCEPTED", ok as u32, GREEN);
    ui_hex(canvas, 560, 1048, b"GATED", gated as u32, AMBER);
    ui_hex(canvas, 104, 1096, b"C0", probe[0], FG);
    ui_hex(canvas, 560, 1096, b"C1", probe[1], FG);
    ui_hex(canvas, 104, 1144, b"C2", probe[2], FG);

    panel(canvas, 72, 1220, 936, 360, PANEL_ALT);
    ui_text(canvas, 104, 1252, b"WATCHDOG SNAPSHOT", MUTED);
    ui_hex(canvas, 104, 1300, b"LOAD", wd.load, FG);
    ui_hex(canvas, 560, 1300, b"VALUE", wd.value, FG);
    ui_hex(canvas, 104, 1348, b"CTRL", wd.control, FG);
    ui_hex(canvas, 560, 1348, b"LOCK", wd.lock, FG);
    ui_text(canvas, 104, 1424, b"RING: DURABLE / UART: MIRRORED", CYAN);
    ui_text(canvas, 104, 1472, b"LAST STAGE: FRAMEBUFFER BLIT", GREEN);
    ui_text(
        canvas,
        104,
        1520,
        b"A PHOTO OF THIS PANEL IS A BOOT RECORD",
        MUTED,
    );

    rect(canvas, 72, 2270, 936, 2, CYAN);
    ui_text(canvas, 72, 2290, b"FULLERENE / ANE", FG);
    ui_text(canvas, 720, 2290, b"PANEL DIAGNOSTICS ACTIVE", MUTED);
}

/// Paint the boot report (USB state + watchdog snapshot + sweep stats) and
/// blit it to every candidate layer. Runs in the boot that performed the
/// bring-up: the writer is also the reporter. Every stage is also mirrored
/// to the console (uart -> durable log) so the v7f reader can show where a
/// silent stop happened even when the panel shows nothing.
pub fn report(usb: &UsbState, log: fn(&str, u64)) {
    if desktop_owned() || irq_quiet() {
        return;
    }
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

    draw_boot_screen(canvas, usb, wd, ok, gated, probe);

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
/// Paint the tail of the durable text ring onto free canvas rows (60..66) so
/// the uart log stays readable from a photo even when the USB channel is
/// dead. The ring's used length = the raw cursor at TEXT_BLOCK_BASE + 4
/// (same guard as the writer: an out-of-range cursor means empty). Called
/// from the IRQ batch, so the six rows refresh a few times per second.
pub fn paint_log_tail() {
    if desktop_owned() || irq_quiet() {
        return;
    }
    use crate::blackbox::{TEXT_BLOCK_BASE, TEXT_BLOCK_CAP};
    let cap = TEXT_BLOCK_CAP as usize;
    let raw = unsafe { core::ptr::read_volatile((TEXT_BLOCK_BASE + 4) as *const u32) } as usize;
    let used = if raw < cap { raw } else { 0 };
    let take = 240usize;
    let start = used.saturating_sub(take);
    for r in 0..6u32 {
        let mut buf = [b' '; 40];
        for (c, slot) in buf.iter_mut().enumerate() {
            let i = start + (r as usize) * 40 + c;
            if i < used {
                let b = unsafe {
                    core::ptr::read_volatile((TEXT_BLOCK_BASE as usize + i) as *const u8)
                };
                *slot = if (0x20..0x7f).contains(&b) { b } else { b'.' };
            }
        }
        paint(60 + r, &buf);
        flush_row(60 + r);
    }
}

pub fn paint(row: u32, text: &[u8]) {
    if desktop_owned() || irq_quiet() {
        return;
    }
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

/// Paint a labelled hex value on one line (rows follow `paint`'s convention).
///
/// no_std: the digits are formatted by hand. Fixed width (16 digits) keeps
/// the on-panel string stable between successive frames.
pub fn paint_hex(row: u32, label: &[u8], value: u64) {
    let mut buf = [0u8; 48];
    let mut n = 0;
    for &b in label {
        if n < buf.len() {
            buf[n] = b;
            n += 1;
        }
    }
    if n < buf.len() {
        buf[n] = b':';
        n += 1;
    }
    for i in 0..16 {
        let nib = ((value >> (4 * (15 - i))) & 0xf) as u8;
        if n < buf.len() {
            buf[n] = if nib < 10 {
                b'0' + nib
            } else {
                b'a' + nib - 10
            };
            n += 1;
        }
    }
    paint(row, &buf[..n]);
}

/// Copy only one text row's band from the canvas to every accepted window.
/// A full flush is ~9.6 MiB; one row band is ~4 KiB per window, cheap enough
/// for every-tick instrument paints.
pub fn flush_row(row: u32) {
    if irq_quiet() {
        return;
    }
    let y0 = DIAG_Y + (10 + row) * LINE_PITCH;
    let y1 = (y0 + GLYPH_H).min(HEIGHT);
    if y1 <= y0 {
        return;
    }
    let start = (y0 as usize) * (WIDTH as usize) * 4;
    let len = ((y1 - y0) as usize) * (WIDTH as usize) * 4;
    for &ch in CHANNELS.iter() {
        for &w in WINDOWS.iter() {
            let addr = unsafe { ptr::read_volatile((ch + w) as *const u32) };
            if blit_ok(addr) {
                unsafe {
                    ptr::copy_nonoverlapping(
                        (CANVAS + start) as *const u8,
                        (addr as usize + start) as *mut u8,
                        len,
                    );
                    crate::blackbox::cache_clean(addr as usize + start, len);
                }
            }
        }
    }
    unsafe {
        crate::blackbox::cache_clean(CANVAS + start, len);
    }
}

/// Fill the whole canvas with the background colour. Called once at boot so
/// rows left by a previous (warm-reset) boot cannot masquerade as current
/// evidence on the panel.
pub fn clear() {
    if desktop_owned() || irq_quiet() {
        return;
    }
    let canvas: &mut [u32] =
        unsafe { core::slice::from_raw_parts_mut(CANVAS as *mut u32, CANVAS_LEN / 4) };
    for px in canvas.iter_mut() {
        *px = BG;
    }
    unsafe {
        crate::blackbox::cache_clean(CANVAS, CANVAS_LEN);
    }
}

/// Paint a text line at an explicit x (the right-column instruments). The
/// cell band it clears is its own, so it never disturbs the left text rows
/// painted by the boot report.
pub fn paint_at(row: u32, x: u32, cols: usize, text: &[u8]) {
    if desktop_owned() || irq_quiet() {
        return;
    }
    let canvas: &mut [u32] =
        unsafe { core::slice::from_raw_parts_mut(CANVAS as *mut u32, CANVAS_LEN / 4) };
    let y = DIAG_Y + (10 + row) * LINE_PITCH;
    let x0 = x as usize;
    let x1 = (x as usize + cols * 8).min(WIDTH as usize);
    for ry in y..(y + GLYPH_H).min(HEIGHT) {
        let base = ry as usize * WIDTH as usize;
        for px in &mut canvas[base + x0..base + x1] {
            *px = BG;
        }
    }
    let mut buf = [b' '; 64];
    let n = text.len().min(64);
    buf[..n].copy_from_slice(&text[..n]);
    lattice::font::render_text(canvas, WIDTH, HEIGHT, WIDTH, x, y, &buf[..n], FG, GLYPH_H);
    unsafe {
        crate::blackbox::cache_clean(CANVAS, CANVAS_LEN);
    }
}

/// One-shot event ledger: the free rows under the instrument column
/// (x = 300, rows 38..57). Each event is painted exactly once at its own
/// row and never rewritten, so one photo shows the whole recent sequence
/// instead of a single churning tail window. The tail window (rows 60+)
/// is now a boot-time one-shot; this ledger is the live channel.
const EVENT_ROW_BASE: u32 = 38;
const EVENT_SLOTS: u32 = 20;
static EVENT_NEXT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

pub fn event(tag: &[u8]) {
    use core::sync::atomic::Ordering;
    let idx = EVENT_NEXT.fetch_add(1, Ordering::Relaxed);
    if idx >= EVENT_SLOTS {
        return;
    }
    // v212: mirror every ledger tag into the durable ring. Two things make
    // this necessary:
    //   1. The panel half has twice been the freeze site in exactly this
    //      boot region (v145: a Line::draw clear loop frozen for minutes;
    //      with v211 unblocking the launchd-entry putc, the boot now walks
    //      straight into this paint_at/flush_row).
    //   2. Under the uart-only bisection build the panel half is skipped
    //      entirely - the ring copy is then the only record of the event
    //      sequence, i.e. the instrument that says whether the event call
    //      itself completed.
    crate::blackbox::text_puts("EVT: ");
    crate::blackbox::text_puts(unsafe { core::str::from_utf8_unchecked(tag) });
    crate::blackbox::text_puts("\n");
    #[cfg(not(fullerene_ane_marks_uart_only))]
    {
        // Two-digit ordinal prefix keeps the order readable in photos.
        let mut buf = [b' '; 27];
        buf[0] = b'0' + ((idx / 10) % 10) as u8;
        buf[1] = b'0' + (idx % 10) as u8;
        buf[2] = b':';
        let n = tag.len().min(buf.len() - 3);
        buf[3..3 + n].copy_from_slice(&tag[..n]);
        let row = EVENT_ROW_BASE + idx;
        paint_at(row, 300, 27, &buf[..3 + n]);
        flush_row(row);
    }
}

/// Paint the previous boot's dying words: scan the durable block for the
/// LAST panic marker and copy the text that followed it onto the free rows
/// above the tail window. The block lives in reserved DRAM and survives the
/// reboot, so the previous kernel's panic file/line is what a photo of the
/// next boot shows - the one channel that needs no USB to read.
pub fn report_previous_panic() {
    const BASE: usize = crate::blackbox::TEXT_BLOCK_BASE as usize;
    const CAP: usize = crate::blackbox::TEXT_BLOCK_CAP as usize;
    let needle = b"FULLERENE AARCH64 PANIC";
    let mut found: usize = usize::MAX;
    let mut i = 0;
    'scan: while i + needle.len() <= CAP {
        let mut j = 0;
        while j < needle.len() {
            let b = unsafe { core::ptr::read_volatile((BASE + i + j) as *const u8) };
            if b != needle[j] {
                i += 1;
                continue 'scan;
            }
            j += 1;
        }
        found = i;
        i += 1;
    }
    if found == usize::MAX {
        crate::blackbox::text_puts("PREV-PANIC: none found\n");
        return;
    }
    // The marker is followed by "PANIC: FILE=... LINE=0X..." (upper-cased on
    // the way into the ring); copy a printable run and paint two rows.
    let mut buf = [b' '; 82];
    let mut n = 0;
    let start = found + needle.len();
    while n < buf.len() && start + n < CAP {
        let b = unsafe { core::ptr::read_volatile((BASE + start + n) as *const u8) };
        if b == 0 {
            break;
        }
        buf[n] = if (0x20..0x7f).contains(&b) { b } else { b' ' };
        n += 1;
    }
    paint_at(58, 0, 40, &buf[..n.min(40)]);
    flush_row(58);
    let hi = n.min(80);
    if hi > 40 {
        paint_at(59, 0, 40, &buf[40..hi]);
        flush_row(59);
    }
    crate::blackbox::text_puts("PREV-PANIC painted rows 58-59\n");
}

/// `paint_hex` at an explicit x on the right column (rows follow `paint`).
pub fn paint_hex_at(row: u32, x: u32, label: &[u8], value: u64) {
    let mut buf = [0u8; 48];
    let mut n = 0;
    for &b in label {
        if n < buf.len() {
            buf[n] = b;
            n += 1;
        }
    }
    if n < buf.len() {
        buf[n] = b':';
        n += 1;
    }
    for i in 0..16 {
        let nib = ((value >> (4 * (15 - i))) & 0xf) as u8;
        if n < buf.len() {
            buf[n] = if nib < 10 {
                b'0' + nib
            } else {
                b'a' + nib - 10
            };
            n += 1;
        }
    }
    paint_at(row, x, 24, &buf[..n]);
}

/// Cached instrument values: the right-column probes are re-painted on every
/// IRQ beat batch so a later left-column repaint (the 44-column clear band
/// overlaps x=300) cannot permanently erase them.
pub static ENTRY_PC: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static RST_REASON: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static LAST_FLT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static LAST_FAR: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static SVC_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
pub static SVC_LAST: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
pub static EL0_IRQS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Repaint the right-column instruments (rows 17-24 at x=300).
pub fn refresh_instruments(irq_beats: u64) {
    if desktop_owned() || irq_quiet() {
        return;
    }
    use core::sync::atomic::Ordering;
    paint_hex_at(17, 300, b"RST", RST_REASON.load(Ordering::Relaxed));
    paint_hex_at(18, 300, b"ENTRY", ENTRY_PC.load(Ordering::Relaxed));
    paint_hex_at(19, 300, b"IRQ", irq_beats);
    let svc = ((SVC_COUNT.load(Ordering::Relaxed) as u64) << 16)
        | SVC_LAST.load(Ordering::Relaxed) as u64;
    paint_hex_at(20, 300, b"SVC", svc);
    paint_hex_at(21, 300, b"EL0IRQ", EL0_IRQS.load(Ordering::Relaxed) as u64);
    let px = canvas_probe(400, 620) as u64;
    paint_hex_at(22, 300, b"UMK", px);
    paint_hex_at(23, 300, b"FLT", LAST_FLT.load(Ordering::Relaxed));
    paint_hex_at(24, 300, b"FAR", LAST_FAR.load(Ordering::Relaxed));
    // v245: the v242/v244 IQ meter showed the whole handler at ~64 ms/IRQ
    // (~100% duty) and this was the tail - the full 9.6 MiB flush_panel per
    // repaint. Flush only the rows the batch touched (8 x ~4 KiB bands);
    // identical pixels, a fraction of the cost.
    for row in 17..=24u32 {
        flush_row(row);
    }
}

/// Read one canvas pixel (the identity mapping keeps VA == PA here). Used by
/// the IRQ-era instrument probes to verify writes made from EL0.
pub fn canvas_probe(y: u32, x: u32) -> u32 {
    let index = (y as usize) * (WIDTH as usize) + (x as usize);
    if index >= CANVAS_LEN / 4 {
        return 0;
    }
    unsafe { ptr::read_volatile((CANVAS + index * 4) as *const u32) }
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
    if desktop_owned() || irq_quiet() {
        return;
    }
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
        d.fflush,
        d.strs,
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
    y += LINE_PITCH;
    // v302: the one-behind probe line. FF=flush runs, FFSTUCK=flush never
    // self-cleared, STR=strings served, EP0STUCK=stale-EPENA events.
    Line::new()
        .s(b"FB8 FF=")
        .x(d.fflush & 0xffff)
        .s(b" FFSTUCK=")
        .x(d.fflush >> 16)
        .s(b" STR=")
        .x(d.strs & 0xffff)
        .s(b" E0STK=")
        .x(d.strs >> 16)
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
