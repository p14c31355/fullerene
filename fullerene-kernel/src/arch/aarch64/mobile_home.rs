//! v330: the phone-native home screen - the first portrait-first UI for
//! FullereneOS. Renders directly onto the ANE canvas with primitive
//! drawing (no font dependency for the first light), so it cannot hang
//! in the glyph-mapping path that froze the desktop render.
//!
//! Layout (all coordinates derived from the canvas dims):
//!   - status bar: a notch-clearing top band (the ANE-LX2J panel has a
//!     wide U notch ~80 px deep, centered ~330 px - the status bar sits
//!     BELOW it and paints only the notch sides)
//!   - app grid: 4 columns x N rows of rounded tiles
//!   - dock: the bottom band with 4 tiles
//!
//! This is the first backend of the reusable mobile shell: every constant
//! derives from the canvas size, so other ARM64 phones reuse it as-is.
//! The notch geometry comes from the panel descriptor (the boe_otm1911a
//! 5.84" 1080x2280), expressed as fractions of the width/height so other
//! panels substitute their own.

use core::sync::atomic::Ordering;

/// Fill a rectangle with a color, clipped to the canvas.
fn fill_rect(fb: &mut [u32], fb_w: u32, fb_h: u32, x: u32, y: u32, w: u32, h: u32, color: u32) {
    let x1 = x.min(fb_w);
    let y1 = y.min(fb_h);
    let x2 = x.saturating_add(w).min(fb_w);
    let y2 = y.saturating_add(h).min(fb_h);
    for row in y1..y2 {
        let base = (row * fb_w) as usize;
        for col in x1..x2 {
            fb[base + col as usize] = color;
        }
    }
}

/// Fill a rounded-rect tile (corner radius r) - simple circle test on the
/// corners, plain rect inside.
fn fill_tile(
    fb: &mut [u32],
    fb_w: u32,
    fb_h: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    r: u32,
    color: u32,
) {
    let x1 = x.min(fb_w);
    let y1 = y.min(fb_h);
    let x2 = x.saturating_add(w).min(fb_w);
    let y2 = y.saturating_add(h).min(fb_h);
    let r2 = r * r;
    for row in y1..y2 {
        let dy_corner_top = if row < y + r { y + r - row } else { 0 };
        let dy_corner_bot = if row >= y2.saturating_sub(r) {
            row - (y2 - r) + 1
        } else {
            0
        };
        let dy = dy_corner_top.max(dy_corner_bot);
        let dx_lim = if dy > 0 {
            let dy2 = dy * dy;
            if dy2 >= r2 {
                0
            } else {
                r - (r2 - dy2).integer_sqrt()
            }
        } else {
            0
        };
        let start = (x + dx_lim).min(x2);
        let end = (x + w).saturating_sub(dx_lim).min(x2);
        if start < end {
            let base = (row * fb_w) as usize;
            for col in start..end {
                fb[base + col as usize] = color;
            }
        }
    }
}

/// Integer square root (Newton) - no std dependency.
trait IntegerSqrt {
    fn integer_sqrt(self) -> Self;
}

impl IntegerSqrt for u32 {
    fn integer_sqrt(self) -> u32 {
        if self < 2 {
            return self;
        }
        let mut x = self;
        let mut y = (x + 1) / 2;
        while y < x {
            x = y;
            y = (x + self / x) / 2;
        }
        x
    }
}

/// The launcher geometry, derived once from the canvas so the renderer,
/// the press feedback and the hit test cannot disagree about a tile.
#[derive(Clone, Copy)]
pub struct HomeLayout {
    pub width: u32,
    pub height: u32,
    pub bar_h: u32,
    pub notch_l: u32,
    pub notch_w: u32,
    pub notch_h: u32,
    pub grid_top: u32,
    pub grid_bottom: u32,
    pub margin: u32,
    pub gap: u32,
    pub tile_w: u32,
    pub dock_top: u32,
}

/// What a touch at a point asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HomeHit {
    App { row: u32, col: u32 },
    Dock(u32),
    None,
}

impl HomeLayout {
    pub fn new(width: u32, height: u32) -> Self {
        let notch_w = width * 330 / 1080;
        let notch_h = height * 80 / 2280;
        let bar_h = notch_h + 56;
        let margin = 40u32;
        let gap = 28u32;
        let tile_w = (width - margin * 2 - gap * 3) / 4;
        let dock_top = height.saturating_sub(200);
        Self {
            width,
            height,
            bar_h,
            notch_l: (width - notch_w) / 2,
            notch_w,
            notch_h,
            grid_top: bar_h + 40,
            grid_bottom: dock_top.saturating_sub(32),
            margin,
            gap,
            tile_w,
            dock_top,
        }
    }

    /// Which tile, if any, covers this panel point. Pure geometry: no
    /// canvas, no state, so it is testable off-device.
    pub fn hit_test(&self, x: u32, y: u32) -> HomeHit {
        let pitch = self.tile_w + self.gap;
        if y >= self.dock_top {
            let dock_tile = 120u32;
            let dock_gap = self.width.saturating_sub(4 * dock_tile) / 5;
            if y >= self.dock_top + (200 - dock_tile) / 2 {
                for i in 0..4u32 {
                    let x0 = dock_gap + i * (dock_tile + dock_gap);
                    if x >= x0 && x < x0 + dock_tile {
                        return HomeHit::Dock(i);
                    }
                }
            }
            return HomeHit::None;
        }
        if y < self.grid_top || y >= self.grid_bottom || x < self.margin {
            return HomeHit::None;
        }
        let col = (x - self.margin) / pitch;
        if col >= 4 {
            return HomeHit::None;
        }
        let rel_y = y - self.grid_top;
        if rel_y % pitch >= self.tile_w {
            return HomeHit::None;
        }
        HomeHit::App {
            row: rel_y / pitch,
            col,
        }
    }

    /// Top-left corner of a grid tile.
    pub fn app_origin(&self, row: u32, col: u32) -> (u32, u32) {
        (
            self.margin + col * (self.tile_w + self.gap),
            self.grid_top + row * (self.tile_w + self.gap),
        )
    }
}

/// Status-bar battery capsule: outline, then a fill proportional to charge.
///
/// The charge comes from the Hi6555V100 coulomb counter via the PMU cell
/// (`platform::ane_battery`), two byte reads, so it is re-sampled on every
/// repaint.  A pack whose voltage cannot be read leaves the capsule empty
/// rather than inventing a percentage.
///
/// A 3x5 digit bitmap, one byte per row, bit 2 = leftmost column. Big
/// enough to scale up into the status bar without dragging in the glyph
/// path (the first light deliberately avoided the font engine).
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111], // 0
    [0b010, 0b110, 0b010, 0b010, 0b111], // 1
    [0b111, 0b001, 0b111, 0b100, 0b111], // 2
    [0b111, 0b001, 0b111, 0b001, 0b111], // 3
    [0b101, 0b101, 0b111, 0b001, 0b001], // 4
    [0b111, 0b100, 0b111, 0b001, 0b111], // 5
    [0b111, 0b100, 0b111, 0b101, 0b111], // 6
    [0b111, 0b001, 0b001, 0b001, 0b001], // 7
    [0b111, 0b101, 0b111, 0b101, 0b111], // 8
    [0b111, 0b101, 0b111, 0b001, 0b111], // 9
];

/// Blit one scaled digit. Returns the width it consumed.
fn draw_digit(fb: &mut [u32], fb_w: u32, fb_h: u32, x: u32, y: u32, d: u8, s: u32, c: u32) -> u32 {
    let glyph = DIGITS[(d % 10) as usize];
    for (row, bits) in glyph.iter().enumerate() {
        for col in 0..3u32 {
            if bits & (1 << (2 - col)) == 0 {
                continue;
            }
            fill_rect(fb, fb_w, fb_h, x + col * s, y + row as u32 * s, s, s, c);
        }
    }
    3 * s
}

/// The charge as two digits, drawn inside the capsule: a dark drop shadow
/// first so the number stays legible over the green / amber / red fill.
fn draw_percent(fb: &mut [u32], fb_w: u32, fb_h: u32, x: u32, y: u32, pct: u32, s: u32) {
    let pct = pct.min(100);
    let tens = (pct / 10) as u8;
    let ones = (pct % 10) as u8;
    let gap = s;
    let shadow = 0xFF00_00_00u32;
    // Shadow (2 px at s=4), then the glyph in white.
    let mut cx = x;
    if pct >= 10 {
        cx += draw_digit(fb, fb_w, fb_h, cx + 1, y + 2, tens, s, shadow) + gap;
    }
    let _ = draw_digit(fb, fb_w, fb_h, cx + 1, y + 2, ones, s, shadow);
    let mut cx = x;
    if pct >= 10 {
        cx += draw_digit(fb, fb_w, fb_h, cx, y, tens, s, 0xFFFF_FFFF) + gap;
    }
    let _ = draw_digit(fb, fb_w, fb_h, cx, y, ones, s, 0xFFFF_FFFF);
}

fn draw_battery(fb: &mut [u32], fb_w: u32, fb_h: u32, l: &HomeLayout) {
    let state = crate::platform::ane_battery::state();
    let w = 62u32;
    let h = 32u32;
    let x = fb_w.saturating_sub(l.margin + w);
    let y = l.notch_h + (l.bar_h - l.notch_h).saturating_sub(h) / 2;
    let outline: u32 = 0xFF8A_94_A0;

    fill_rect(fb, fb_w, fb_h, x, y, w, h, outline);
    fill_rect(fb, fb_w, fb_h, x + 2, y + 2, w - 4, h - 4, 0xFF10_14_18);
    // The nub on the right edge, so the capsule reads as a battery.
    fill_rect(fb, fb_w, fb_h, x + w, y + 6, 3, h - 12, outline);

    let Some(permille) = state.charge_permille else {
        return;
    };
    let inner = w - 4;
    let filled = inner * permille.min(1000) as u32 / 1000;
    let color = if state.charging {
        0xFF4C_8D_FF
    } else if permille > 500 {
        0xFF4C_AF_50
    } else if permille > 200 {
        0xFFE0_A0_30
    } else {
        0xFFE0_40_40
    };
    fill_rect(fb, fb_w, fb_h, x + 2, y + 2, filled, h - 4, color);

    // The number, centred in the capsule (2 digits at scale 4 = 28x20).
    let pct = (permille.min(1000) as u32 + 5) / 10;
    let s = 4u32;
    let text_w = if pct >= 10 { 2 * (3 * s) + s } else { 3 * s };
    let tx = x + 2 + (inner - text_w) / 2;
    let ty = y + 2 + (h - 4 - 5 * s) / 2;
    draw_percent(fb, fb_w, fb_h, tx, ty, pct, s);
}

/// Render the phone home screen onto the canvas. Every color/position
/// derives from the canvas dimensions.
pub fn render_home() {
    let width = crate::platform::ane_fb::WIDTH;
    let height = crate::platform::ane_fb::HEIGHT;
    let l = HomeLayout::new(width, height);
    let canvas_ptr = crate::platform::ane_fb::CANVAS as *mut u32;
    let count = (width as usize) * (height as usize);
    let fb = unsafe { core::slice::from_raw_parts_mut(canvas_ptr, count) };

    // v331k: the mobile shell owns the panel from this moment - the boot
    // strip, the S-value diag, and the uart mirror all stop painting (the
    // ane_fb paint paths early-return on DESKTOP_OWNED). The diagnostics
    // live in the ring.
    crate::platform::ane_fb::DESKTOP_OWNED.store(true, Ordering::Relaxed);

    // Palette (Android-ish dark theme).
    let bg: u32 = 0xFF10_14_18;
    let bar: u32 = 0xFF1A_1E_24;
    let tile: u32 = 0xFF2A_30_38;
    let tile_hi: u32 = 0xFF3A_44_50;
    let accent: u32 = 0xFF4C_8D_FF;
    let dock: u32 = 0xFF16_1A_20;

    // Background.
    fill_rect(fb, width, height, 0, 0, width, height, bg);

    // v331i: the notch geometry. The ANE-LX2J (boe_otm1911a 5.84")
    // carries a wide U notch: ~330 px wide, ~80 px deep, centered. The
    // reusable shell expresses it in fractions of the panel so other
    // phones substitute their own descriptor values.
    let notch_w = l.notch_w;
    let notch_h = l.notch_h;
    let notch_l = l.notch_l;
    let notch_r = notch_l + notch_w;

    // Status bar: the full-width band. Its content row sits BELOW the
    // notch row: the band covers the notch depth plus the content strip,
    // and the notch rectangle itself is left as background (the cutout).
    let bar_h = l.bar_h;
    fill_rect(fb, width, height, 0, 0, width, bar_h, bar);
    fill_rect(fb, width, height, notch_l, 0, notch_w, notch_h, bg);
    draw_battery(fb, width, height, &l);

    // App grid: 4 cols x N rows of tiles between the bars.
    let grid_top = bar_h + 40;
    let cols = 4u32;
    let rows = 5u32;
    let margin = l.margin;
    let gap = l.gap;
    let tile_w = l.tile_w;
    let tile_h = tile_w; // square tiles
    let dock_top = l.dock_top;
    let grid_bottom = l.grid_bottom;
    let row_pitch = tile_h + gap;
    for row in 0..rows {
        let y = grid_top + row * row_pitch;
        if y + tile_h > grid_bottom {
            break;
        }
        for col in 0..cols {
            let x = margin + col * (tile_w + gap);
            // Alternate shades so the grid is visible without fonts.
            let color = if (row + col) % 2 == 0 { tile } else { tile_hi };
            fill_tile(fb, width, height, x, y, tile_w, tile_h, 24, color);
        }
    }

    // Dock band with 4 tiles.
    fill_rect(fb, width, height, 0, dock_top, width, 200, dock);
    let dock_tile = 120u32;
    let dock_gap = (width - 4 * dock_tile) / 5;
    for i in 0..4u32 {
        let x = dock_gap + i * (dock_tile + dock_gap);
        let y = dock_top + (200 - dock_tile) / 2;
        fill_tile(fb, width, height, x, y, dock_tile, dock_tile, 28, accent);
    }

    // A thin accent line under the status bar (skipping the notch span).
    fill_rect(fb, width, height, 0, bar_h, notch_l, 4, accent);
    fill_rect(
        fb,
        width,
        height,
        notch_r,
        bar_h,
        width - notch_r,
        4,
        accent,
    );
}

/// Where the last poll put the finger, in panel pixels.
pub fn touch_point() -> Option<(u32, u32)> {
    let (x, y) = crate::platform::ane_touch::snapshot(0)?;
    Some((x as u32, y as u32))
}

// ── v347: the on-screen keyboard ────────────────────────────────────────
//
// The dock's fourth tile is the Notes app: a full-screen text view over
// the home screen with the lattice IME docked at the bottom. The shell
// owns a `lattice::ime::Keyboard` + `Field` pair in a spin mutex (the
// touch path is IRQ-adjacent, the render path is the boot thread), and
// every touch inside the keyboard area goes through
// `Keyboard::press_at` -> `Field::apply` -> repaint.

use lattice::ime::{Field as ImeField, ImeStyle, Keyboard as ImeKeyboard};

/// The keyboard's docked height, as a fraction of the panel: the lattice
/// rows want ~340 px on a 1080-wide panel (4 rows at ~80 px + gaps).
const IME_HEIGHT: u32 = 380;
/// Notes text line pitch (the 3x5 digit font is too coarse for prose; the
/// lattice bitmap font is 8x13 at this scale).
const NOTES_LINE_PITCH: u32 = 40;

/// The shell's IME state. A spin mutex because both the touch poller and
/// the repaint path touch it and neither may sleep.
static NOTES_IMES: spin::Mutex<NotesIme> = spin::Mutex::new(NotesIme::new());

/// Notes app state: keyboard + text field, plus whether the app is open.
/// The `Field` is `Option` because `String` has no const constructor; it
/// is created on the first open (heap is live by then).
pub struct NotesIme {
    pub keyboard: ImeKeyboard,
    pub field: Option<ImeField>,
    /// True once the Notes tile has been tapped; the home screen renders
    /// underneath otherwise.
    pub open: bool,
}

impl NotesIme {
    const fn new() -> Self {
        Self {
            keyboard: ImeKeyboard::new(),
            field: None,
            open: false,
        }
    }

    /// The field, created on first use.
    fn field_mut(&mut self) -> &mut ImeField {
        self.field.get_or_insert_with(ImeField::new)
    }
}

/// The keyboard rectangle on the canvas. A free function so the hit test,
/// the renderer and the tests cannot disagree.
pub fn ime_area(l: &HomeLayout) -> (u32, u32, u32, u32) {
    let top = l.height.saturating_sub(IME_HEIGHT);
    (0, top, l.width, IME_HEIGHT)
}

/// True when the point is inside the docked keyboard.
fn in_ime(l: &HomeLayout, x: u32, y: u32) -> bool {
    let (ax, ay, aw, ah) = ime_area(l);
    x >= ax && x < ax + aw && y >= ay && y < ay + ah
}

/// Feed a tap (or a sample while pressed) into the keyboard. Called from
/// the touch edge in the wait loop. Returns true when the state changed
/// enough to want a repaint.
pub fn ime_press(x: u32, y: u32) -> bool {
    let width = crate::platform::ane_fb::WIDTH;
    let height = crate::platform::ane_fb::HEIGHT;
    let l = HomeLayout::new(width, height);
    let (ax, ay, aw, ah) = ime_area(&l);
    let mut ime = NOTES_IMES.lock();
    let Some(action) = ime.keyboard.press_at(
        lattice::ime::Rect::new(ax as i32, ay as i32, aw as i32, ah as i32),
        x as i32,
        y as i32,
    ) else {
        return false;
    };
    ime.field_mut().apply(action)
}

/// The finger left the panel: clear the pressed key.
pub fn ime_release() {
    NOTES_IMES.lock().keyboard.release();
}

/// Notes is open?
pub fn notes_open() -> bool {
    NOTES_IMES.lock().open
}

/// Open or close the Notes app (the dock tap). Returns true on a change.
pub fn toggle_notes(open: bool) -> bool {
    let mut ime = NOTES_IMES.lock();
    if ime.open == open {
        return false;
    }
    ime.open = open;
    true
}

/// The notes text, copied out for tests / getvar.
pub fn notes_text() -> alloc::string::String {
    match NOTES_IMES.lock().field.as_ref() {
        Some(f) => f.text().into(),
        None => alloc::string::String::new(),
    }
}

/// Draw a bitmap-font text line (the lattice font's 8-wide glyphs), clipped
/// to the canvas.
fn draw_text_line(fb: &mut [u32], fb_w: u32, fb_h: u32, x: u32, y: u32, text: &[u8], color: u32) {
    lattice::font::render_text(fb, fb_w, fb_h, fb_w, x, y, text, color, 13);
}

/// Render the Notes app: the text field above, the IME docked below.
/// Called instead of `render_home` while the app is open.
pub fn render_notes() {
    let width = crate::platform::ane_fb::WIDTH;
    let height = crate::platform::ane_fb::HEIGHT;
    let l = HomeLayout::new(width, height);
    let canvas_ptr = crate::platform::ane_fb::CANVAS as *mut u32;
    let count = (width as usize) * (height as usize);
    let fb = unsafe { core::slice::from_raw_parts_mut(canvas_ptr, count) };

    crate::platform::ane_fb::DESKTOP_OWNED.store(true, Ordering::Relaxed);

    let bg: u32 = 0xFF10_14_18;
    let bar: u32 = 0xFF1A_1E_24;
    let text: u32 = 0xFFE8_E8_F0;
    fill_rect(fb, width, height, 0, 0, width, height, bg);
    // Title band, notch-aware like the status bar.
    fill_rect(fb, width, height, 0, 0, width, l.bar_h, bar);
    draw_text_line(fb, width, height, l.margin, l.notch_h + 8, b"Notes", text);

    // The text field: the caret line plus the text above it, word-wrapped
    // to the panel width (the IME owns the bottom band).
    let ime_top = height.saturating_sub(IME_HEIGHT);
    let mut ime = NOTES_IMES.lock();
    let area = lattice::ime::Rect::new(0, ime_top as i32, width as i32, IME_HEIGHT as i32);
    ime.keyboard.draw(
        &mut lattice::painter::Painter::new(fb, width, height),
        area,
        &ImeStyle::dark(),
    );

    // Wrap the field's text into lines of (width - 2*margin) / 8 glyphs.
    let cols = ((width - l.margin * 2) / 8) as usize;
    let body_top = l.bar_h + 16;
    let body_bottom = ime_top.saturating_sub(8);
    let max_lines = body_bottom.saturating_sub(body_top) / NOTES_LINE_PITCH;
    let mut line = [0u8; 64];
    let mut line_len = 0usize;
    let mut row = 0u32;
    let empty = "";
    let bytes = ime.field.as_ref().map_or(empty, |f| f.text()).as_bytes();
    let caret = ime.field.as_ref().map_or(0, |f| f.caret());
    let mut caret_row = 0u32;
    let mut caret_col = 0usize;
    for (bi, &b) in bytes.iter().enumerate() {
        if bi == caret {
            caret_row = row;
            caret_col = line_len;
        }
        if b == b'\n' {
            if row < max_lines {
                draw_text_line(fb, width, height, l.margin, body_top + row * NOTES_LINE_PITCH, &line[..line_len], text);
            }
            row += 1;
            line_len = 0;
            continue;
        }
        if line_len == cols {
            if row < max_lines {
                draw_text_line(fb, width, height, l.margin, body_top + row * NOTES_LINE_PITCH, &line[..line_len], text);
            }
            row += 1;
            line_len = 0;
        }
        if line_len < line.len() {
            line[line_len] = b;
            line_len += 1;
        }
    }
    if bi_caret_after_loop(bytes.len(), caret) {
        caret_row = row;
        caret_col = line_len;
    }
    if row < max_lines {
        draw_text_line(fb, width, height, l.margin, body_top + row * NOTES_LINE_PITCH, &line[..line_len], text);
        // The caret: a block the width of one glyph on the caret cell.
        if caret_row == row {
            let cx = l.margin + caret_col as u32 * 8;
            let cy = body_top + caret_row * NOTES_LINE_PITCH + 16;
            fill_rect(fb, width, height, cx, cy, 8, 13, 0xFF4C_8D_FF);
        }
    }
}

/// The caret position is only known to be "after the last byte" when the
/// loop never hit `bi == caret`; a free function so the condition is named.
fn bi_caret_after_loop(len: usize, caret: usize) -> bool {
    caret >= len
}

/// v331t: press feedback - repaint the tile the finger is on in the
/// accent color. Returns the hit so the caller can decide what a
/// release on it means, instead of the geometry being re-derived there.
pub fn touch_feedback() -> HomeHit {
    let width = crate::platform::ane_fb::WIDTH;
    let height = crate::platform::ane_fb::HEIGHT;
    let l = HomeLayout::new(width, height);
    let Some((x, y)) = touch_point() else {
        return HomeHit::None;
    };
    let hit = l.hit_test(x, y);
    let (tx, ty) = match hit {
        HomeHit::App { row, col } => l.app_origin(row, col),
        // Dock presses keep their tile lit too; the dock origin is the
        // same arithmetic with the dock pitch.
        HomeHit::Dock(i) => {
            let dock_tile = 120u32;
            let dock_gap = width.saturating_sub(4 * dock_tile) / 5;
            (
                dock_gap + i * (dock_tile + dock_gap),
                l.dock_top + (200 - dock_tile) / 2,
            )
        }
        HomeHit::None => return hit,
    };
    let canvas_ptr = crate::platform::ane_fb::CANVAS as *mut u32;
    let count = (width as usize) * (height as usize);
    let fb = unsafe { core::slice::from_raw_parts_mut(canvas_ptr, count) };
    let side = if let HomeHit::App { .. } = hit {
        l.tile_w
    } else {
        120
    };
    fill_tile(fb, width, height, tx, ty, side, side, 24, 0xFF4C_8D_FF);
    hit
}
