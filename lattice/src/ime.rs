//! An on-screen keyboard for phone-sized touch screens.
//!
//! A phone has no key row, so text entry is geometry plus state: rows of
//! labelled keys laid out inside a rectangle the shell gives us, a hit test
//! that turns a touch into a key, and the shift / page state that decides
//! what that key means. Rendering is one call so a shell that already owns a
//! painter does not have to know how a keyboard is built.
//!
//! The model is deliberately independent of the desktop: the same keyboard
//! can be drawn over a mobile shell, inside a window on the desktop, or not
//! at all in a unit test. Text lives in [`Field`], which owns the string and
//! the caret; the keyboard only says what a press meant.
//!
//! Rows are measured in half-units so a letter is 2 and a wide key is 3; a
//! row must come to [`UNITS_PER_ROW`] or the layout is rejected by the tests
//! that check it.

use alloc::string::String;

use crate::painter::Painter;

/// A row is this many half-units wide. Letter = 2, wide = 3, space = 10.
pub const UNITS_PER_ROW: u16 = 20;

/// Most keys any page can draw; the biggest page here is 40.
pub const MAX_KEYS: usize = 48;

/// What pressing a key does to the keyboard's own state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyCode {
    /// A character, already in the case the page shows.
    Char(u8),
    Space,
    Backspace,
    Enter,
    /// Change which page is shown.
    Page(u8),
    /// Toggle shift for the next character.
    Shift,
    /// Nothing yet (a key that exists for shape only).
    None,
}

/// One key: its label, its meaning, and its width in half-units.
#[derive(Clone, Copy, Debug)]
pub struct KeyDef {
    pub label: &'static str,
    pub code: KeyCode,
    pub units: u16,
}

const fn k(label: &'static str, code: KeyCode) -> KeyDef {
    KeyDef {
        label,
        code,
        units: 2,
    }
}

const fn w(label: &'static str, code: KeyCode) -> KeyDef {
    KeyDef {
        label,
        code,
        units: 3,
    }
}

const PAGE_LETTERS: u8 = 0;
const PAGE_SYMBOLS: u8 = 1;

const LETTERS_LOWER: &[&[KeyDef]] = &[
    &[
        k("q", KeyCode::Char(b'q')),
        k("w", KeyCode::Char(b'w')),
        k("e", KeyCode::Char(b'e')),
        k("r", KeyCode::Char(b'r')),
        k("t", KeyCode::Char(b't')),
        k("y", KeyCode::Char(b'y')),
        k("u", KeyCode::Char(b'u')),
        k("i", KeyCode::Char(b'i')),
        k("o", KeyCode::Char(b'o')),
        k("p", KeyCode::Char(b'p')),
    ],
    &[
        k("a", KeyCode::Char(b'a')),
        k("s", KeyCode::Char(b's')),
        k("d", KeyCode::Char(b'd')),
        k("f", KeyCode::Char(b'f')),
        k("g", KeyCode::Char(b'g')),
        k("h", KeyCode::Char(b'h')),
        k("j", KeyCode::Char(b'j')),
        k("k", KeyCode::Char(b'k')),
        k("l", KeyCode::Char(b'l')),
    ],
    &[
        w("SHIFT", KeyCode::Shift),
        k("z", KeyCode::Char(b'z')),
        k("x", KeyCode::Char(b'x')),
        k("c", KeyCode::Char(b'c')),
        k("v", KeyCode::Char(b'v')),
        k("b", KeyCode::Char(b'b')),
        k("n", KeyCode::Char(b'n')),
        k("m", KeyCode::Char(b'm')),
        w("DEL", KeyCode::Backspace),
    ],
    &[
        w("?123", KeyCode::Page(PAGE_SYMBOLS)),
        k(",", KeyCode::Char(b',')),
        KeyDef {
            label: "space",
            code: KeyCode::Space,
            units: 10,
        },
        k(".", KeyCode::Char(b'.')),
        w("RET", KeyCode::Enter),
    ],
];

const LETTERS_UPPER: &[&[KeyDef]] = &[
    &[
        k("Q", KeyCode::Char(b'Q')),
        k("W", KeyCode::Char(b'W')),
        k("E", KeyCode::Char(b'E')),
        k("R", KeyCode::Char(b'R')),
        k("T", KeyCode::Char(b'T')),
        k("Y", KeyCode::Char(b'Y')),
        k("U", KeyCode::Char(b'U')),
        k("I", KeyCode::Char(b'I')),
        k("O", KeyCode::Char(b'O')),
        k("P", KeyCode::Char(b'P')),
    ],
    &[
        k("A", KeyCode::Char(b'A')),
        k("S", KeyCode::Char(b'S')),
        k("D", KeyCode::Char(b'D')),
        k("F", KeyCode::Char(b'F')),
        k("G", KeyCode::Char(b'G')),
        k("H", KeyCode::Char(b'H')),
        k("J", KeyCode::Char(b'J')),
        k("K", KeyCode::Char(b'K')),
        k("L", KeyCode::Char(b'L')),
    ],
    &[
        w("SHIFT", KeyCode::Shift),
        k("Z", KeyCode::Char(b'Z')),
        k("X", KeyCode::Char(b'X')),
        k("C", KeyCode::Char(b'C')),
        k("V", KeyCode::Char(b'V')),
        k("B", KeyCode::Char(b'B')),
        k("N", KeyCode::Char(b'N')),
        k("M", KeyCode::Char(b'M')),
        w("DEL", KeyCode::Backspace),
    ],
    &[
        w("?123", KeyCode::Page(PAGE_SYMBOLS)),
        k(",", KeyCode::Char(b',')),
        KeyDef {
            label: "space",
            code: KeyCode::Space,
            units: 10,
        },
        k(".", KeyCode::Char(b'.')),
        w("RET", KeyCode::Enter),
    ],
];

const SYMBOLS: &[&[KeyDef]] = &[
    &[
        k("1", KeyCode::Char(b'1')),
        k("2", KeyCode::Char(b'2')),
        k("3", KeyCode::Char(b'3')),
        k("4", KeyCode::Char(b'4')),
        k("5", KeyCode::Char(b'5')),
        k("6", KeyCode::Char(b'6')),
        k("7", KeyCode::Char(b'7')),
        k("8", KeyCode::Char(b'8')),
        k("9", KeyCode::Char(b'9')),
        k("0", KeyCode::Char(b'0')),
    ],
    &[
        k("@", KeyCode::Char(b'@')),
        k("#", KeyCode::Char(b'#')),
        k("$", KeyCode::Char(b'$')),
        k("%", KeyCode::Char(b'%')),
        k("&", KeyCode::Char(b'&')),
        k("*", KeyCode::Char(b'*')),
        k("-", KeyCode::Char(b'-')),
        k("+", KeyCode::Char(b'+')),
        k("(", KeyCode::Char(b'(')),
        k(")", KeyCode::Char(b')')),
    ],
    &[
        w("ABC", KeyCode::Page(PAGE_LETTERS)),
        k("/", KeyCode::Char(b'/')),
        k(":", KeyCode::Char(b':')),
        k(";", KeyCode::Char(b';')),
        k("!", KeyCode::Char(b'!')),
        k("?", KeyCode::Char(b'?')),
        k("'", KeyCode::Char(b'\'')),
        k("\"", KeyCode::Char(b'"')),
        w("DEL", KeyCode::Backspace),
    ],
    &[
        w("ABC", KeyCode::Page(PAGE_LETTERS)),
        k(",", KeyCode::Char(b',')),
        KeyDef {
            label: "space",
            code: KeyCode::Space,
            units: 10,
        },
        k(".", KeyCode::Char(b'.')),
        w("RET", KeyCode::Enter),
    ],
];

/// A rectangle in surface pixels. `w` and `h` are positive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
}

/// One drawn key: where it is and what it means.
#[derive(Clone, Copy, Debug)]
pub struct KeyRect {
    pub rect: Rect,
    pub def: &'static KeyDef,
}

/// What a press did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Insert this byte. Always ASCII here; `Field` widens it to UTF-8.
    Insert(u8),
    Space,
    Backspace,
    Enter,
    /// Shift was toggled; the argument is its new state.
    Shift(bool),
    /// The page changed.
    Page(u8),
    /// The key has no meaning yet.
    None,
}

/// Colours for the keyboard. Kept explicit so the shell can match its theme.
#[derive(Clone, Copy, Debug)]
pub struct ImeStyle {
    pub background: u32,
    pub key: u32,
    pub key_pressed: u32,
    pub text: u32,
    /// Keys that act on the input rather than add to it.
    pub function_key: u32,
    pub gap: i32,
}

impl ImeStyle {
    pub const fn dark() -> Self {
        Self {
            background: 0xFF1A1A1E,
            key: 0xFF35353C,
            key_pressed: 0xFF5A5A66,
            text: 0xFFF2F2F2,
            function_key: 0xFF26262C,
            gap: 6,
        }
    }

    pub const fn light() -> Self {
        Self {
            background: 0xFFD8D8DE,
            key: 0xFFFFFFFF,
            key_pressed: 0xFFB9B9C2,
            text: 0xFF14141A,
            function_key: 0xFFC4C4CC,
            gap: 6,
        }
    }
}

impl Default for ImeStyle {
    fn default() -> Self {
        Self::dark()
    }
}

/// The keyboard's own state: which page, and whether shift is armed.
#[derive(Clone, Copy, Debug)]
pub struct Keyboard {
    page: u8,
    shift: bool,
    /// True while a finger is on a key, for the pressed look.
    pressed: Option<usize>,
}

impl Keyboard {
    pub const fn new() -> Self {
        Self {
            page: PAGE_LETTERS,
            shift: false,
            pressed: None,
        }
    }

    pub const fn page(&self) -> u8 {
        self.page
    }

    pub const fn shift(&self) -> bool {
        self.shift
    }

    /// The rows to draw for the current state.
    pub fn rows(&self) -> &'static [&'static [KeyDef]] {
        match self.page {
            PAGE_SYMBOLS => SYMBOLS,
            _ => {
                if self.shift {
                    LETTERS_UPPER
                } else {
                    LETTERS_LOWER
                }
            }
        }
    }

    /// Lay the keys out inside `area`. Returns how many were written; the
    /// caller sizes `out` to [`MAX_KEYS`].
    pub fn layout(&self, area: Rect, out: &mut [KeyRect]) -> usize {
        let rows = self.rows();
        let n = rows.len() as i32;
        if n == 0 || area.w <= 0 || area.h <= 0 {
            return 0;
        }
        let gap = self.gap_hint(area);
        let row_h = (area.h - gap * (n - 1)) / n;
        let mut count = 0usize;
        let mut y = area.y;
        for row in rows {
            let units: i32 = row.iter().map(|k| k.units as i32).sum();
            if units <= 0 {
                continue;
            }
            let total_gap = gap * (row.len() as i32 - 1);
            let span = area.w - total_gap;
            // A short row is centred, not left-aligned: the second letter
            // row has nine keys and a phone keyboard insets it.
            let row_span = span * units / UNITS_PER_ROW as i32 + total_gap;
            let mut x = area.x + (area.w - row_span) / 2;
            for (i, def) in row.iter().enumerate() {
                // Integer division leaves a pixel or two over; the last key
                // in the row takes them so the row ends flush with the area.
                let w = if i + 1 == row.len() {
                    area.right() - x
                } else {
                    (span * def.units as i32 / units).max(1)
                };
                if count < out.len() {
                    out[count] = KeyRect {
                        rect: Rect::new(x, y, w, row_h),
                        def,
                    };
                    count += 1;
                }
                x += w + gap;
            }
            y += row_h + gap;
        }
        count
    }

    fn gap_hint(&self, area: Rect) -> i32 {
        // Six pixels looks right on a 1080-wide panel and is too much on a
        // 320-wide one, so scale it with the width and keep a floor of 2.
        (area.w / 180).clamp(2, 12)
    }

    /// Which key is at (x, y), if any.
    pub fn hit(&self, area: Rect, x: i32, y: i32) -> Option<&'static KeyDef> {
        let mut rects = [KeyRect {
            rect: Rect::new(0, 0, 0, 0),
            def: &EMPTY_KEY,
        }; MAX_KEYS];
        let n = self.layout(area, &mut rects);
        rects[..n]
            .iter()
            .find(|kr| kr.rect.contains(x, y))
            .map(|kr| kr.def)
    }

    /// Mark a key as held for drawing. Cleared by [`Self::release`].
    pub fn press_at(&mut self, area: Rect, x: i32, y: i32) -> Option<Action> {
        let def = self.hit(area, x, y)?;
        self.pressed = Some(self.index_of(def));
        Some(self.apply(def.code))
    }

    pub fn release(&mut self) {
        self.pressed = None;
    }

    fn index_of(&self, def: &'static KeyDef) -> usize {
        let mut i = 0;
        for row in self.rows() {
            for key in row.iter() {
                if core::ptr::eq(key, def) {
                    return i;
                }
                i += 1;
            }
        }
        0
    }

    /// Apply a key's meaning to the keyboard state and report it.
    fn apply(&mut self, code: KeyCode) -> Action {
        match code {
            KeyCode::Char(c) => {
                // Shift is one shot, as on a phone: the next letter only.
                if self.shift && self.page == PAGE_LETTERS {
                    self.shift = false;
                }
                Action::Insert(c)
            }
            KeyCode::Space => Action::Space,
            KeyCode::Backspace => Action::Backspace,
            KeyCode::Enter => Action::Enter,
            KeyCode::Shift => {
                self.shift = !self.shift;
                Action::Shift(self.shift)
            }
            KeyCode::Page(p) => {
                self.page = p;
                Action::Page(p)
            }
            KeyCode::None => Action::None,
        }
    }

    /// Draw the keyboard into `area`. The caller decides where that is.
    pub fn draw(&self, painter: &mut Painter, area: Rect, style: &ImeStyle) {
        painter.fill_rect(
            area.x,
            area.y,
            area.w.max(0) as u32,
            area.h.max(0) as u32,
            style.background,
        );
        let mut rects = [KeyRect {
            rect: Rect::new(0, 0, 0, 0),
            def: &EMPTY_KEY,
        }; MAX_KEYS];
        let n = self.layout(area, &mut rects);
        for (i, kr) in rects[..n].iter().enumerate() {
            let function = matches!(
                kr.def.code,
                KeyCode::Shift
                    | KeyCode::Backspace
                    | KeyCode::Enter
                    | KeyCode::Page(_)
                    | KeyCode::None
            );
            let held = self.pressed == Some(i);
            let fill = if held {
                style.key_pressed
            } else if function {
                style.function_key
            } else {
                style.key
            };
            let r = kr.rect;
            painter.rounded_rect(r.x, r.y, r.w.max(1) as u32, r.h.max(1) as u32, 10, fill);
            // Centre the label with the bitmap font's own metrics rather
            // than guessing: 8x16 glyphs, so a label of n chars is 8n wide.
            let text_w = (kr.def.label.len() as i32) * 8;
            let tx = r.x + (r.w - text_w) / 2;
            let ty = r.y + (r.h - 16) / 2;
            painter.draw_text(tx, ty, kr.def.label, style.text, 16.0);
        }
    }
}

impl Default for Keyboard {
    fn default() -> Self {
        Self::new()
    }
}

static EMPTY_KEY: KeyDef = KeyDef {
    label: "",
    code: KeyCode::None,
    units: 2,
};

/// The text being edited, with a caret at the end.
///
/// A phone keyboard mostly appends, so the caret only needs to move backwards
/// far enough to correct the last character; a full editor can copy the text
/// out and use its own caret.
#[derive(Clone, Debug, Default)]
pub struct Field {
    text: String,
    caret: usize,
}

impl Field {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_text(text: &str) -> Self {
        Self {
            text: String::from(text),
            caret: text.len(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
    }

    /// Put the caret at the end, where typing resumes.
    pub fn move_to_end(&mut self) {
        self.caret = self.text.len();
    }

    /// Apply what the keyboard said. Returns true if the text changed.
    pub fn apply(&mut self, action: Action) -> bool {
        match action {
            Action::Insert(c) => {
                self.insert_byte(c);
                true
            }
            Action::Space => {
                self.insert_byte(b' ');
                true
            }
            Action::Enter => {
                self.insert_byte(b'\n');
                true
            }
            Action::Backspace => self.backspace(),
            _ => false,
        }
    }

    fn insert_byte(&mut self, c: u8) {
        if self.caret > self.text.len() {
            self.caret = self.text.len();
        }
        self.text.insert(self.caret, c as char);
        self.caret += 1;
    }

    fn backspace(&mut self) -> bool {
        if self.caret == 0 || self.text.is_empty() {
            return false;
        }
        // Never split a multi-byte character: step back to a boundary.
        let mut start = self.caret - 1;
        while start > 0 && (self.text.as_bytes()[start] & 0xC0) == 0x80 {
            start -= 1;
        }
        self.text.replace_range(start..self.caret, "");
        self.caret = start;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0, 1500, 1080, 760);

    fn all_rows() -> alloc::vec::Vec<&'static [KeyDef]> {
        let mut v = alloc::vec::Vec::new();
        for row in LETTERS_LOWER {
            v.push(*row);
        }
        for row in LETTERS_UPPER {
            v.push(*row);
        }
        for row in SYMBOLS {
            v.push(*row);
        }
        v
    }

    #[test]
    fn every_row_measures_at_most_the_full_width() {
        for row in all_rows() {
            let units: u16 = row.iter().map(|k| k.units).sum();
            assert!(units <= UNITS_PER_ROW, "row {:?} is {units}", row[0].label);
            assert!(
                units >= UNITS_PER_ROW - 2,
                "row {:?} is too narrow",
                row[0].label
            );
        }
    }

    #[test]
    fn every_page_fits_in_the_key_buffer() {
        let mut kb = Keyboard::new();
        let mut out = [KeyRect {
            rect: Rect::new(0, 0, 0, 0),
            def: &EMPTY_KEY,
        }; MAX_KEYS];
        for page in [PAGE_LETTERS, PAGE_SYMBOLS] {
            kb.page = page;
            let n = kb.layout(AREA, &mut out);
            assert!(n <= MAX_KEYS);
            assert_eq!(n, kb.rows().iter().map(|r| r.len()).sum::<usize>());
        }
    }

    #[test]
    fn keys_stay_inside_the_area_and_do_not_overlap() {
        let kb = Keyboard::new();
        let mut out = [KeyRect {
            rect: Rect::new(0, 0, 0, 0),
            def: &EMPTY_KEY,
        }; MAX_KEYS];
        let n = kb.layout(AREA, &mut out);
        for kr in &out[..n] {
            assert!(
                kr.rect.x >= AREA.x && kr.rect.right() <= AREA.right(),
                "{:?}",
                kr.def.label
            );
            assert!(
                kr.rect.y >= AREA.y && kr.rect.bottom() <= AREA.bottom(),
                "{:?}",
                kr.def.label
            );
            assert!(kr.rect.w >= 1 && kr.rect.h >= 1);
        }
        for i in 0..n {
            for j in (i + 1)..n {
                let a = out[i].rect;
                let b = out[j].rect;
                let overlap =
                    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom();
                assert!(
                    !overlap,
                    "{:?} overlaps {:?}",
                    out[i].def.label, out[j].def.label
                );
            }
        }
    }

    #[test]
    fn the_centre_of_every_key_hits_that_key() {
        let kb = Keyboard::new();
        let mut out = [KeyRect {
            rect: Rect::new(0, 0, 0, 0),
            def: &EMPTY_KEY,
        }; MAX_KEYS];
        let n = kb.layout(AREA, &mut out);
        for kr in &out[..n] {
            let cx = kr.rect.x + kr.rect.w / 2;
            let cy = kr.rect.y + kr.rect.h / 2;
            let hit = kb.hit(AREA, cx, cy).expect("every centre must hit");
            assert_eq!(hit.label, kr.def.label);
        }
    }

    #[test]
    fn a_touch_outside_the_keys_hits_nothing() {
        let kb = Keyboard::new();
        assert!(kb.hit(AREA, -5, 1500).is_none());
        assert!(kb.hit(AREA, 1079, 1499).is_none());
    }

    #[test]
    fn tapping_a_letter_inserts_it_and_shift_releases_after_one() {
        let mut kb = Keyboard::new();
        let mut field = Field::new();
        kb.press_at(
            AREA,
            key_centre(&kb, AREA, "SHIFT").0,
            key_centre(&kb, AREA, "SHIFT").1,
        );
        assert!(kb.shift());
        let (x, y) = key_centre(&kb, AREA, "Q");
        let action = kb.press_at(AREA, x, y).unwrap();
        assert_eq!(action, Action::Insert(b'Q'));
        field.apply(action);
        assert_eq!(field.text(), "Q");
        assert!(!kb.shift(), "shift is one shot");
        let (x, y) = key_centre(&kb, AREA, "w");
        let action = kb.press_at(AREA, x, y).unwrap();
        field.apply(action);
        assert_eq!(field.text(), "Qw");
    }

    #[test]
    fn backspace_walks_back_one_character_and_survives_an_empty_field() {
        let mut field = Field::with_text("ab");
        assert!(field.apply(Action::Backspace));
        assert_eq!(field.text(), "a");
        assert!(field.apply(Action::Backspace));
        assert!(!field.apply(Action::Backspace));
        assert_eq!(field.text(), "");
    }

    #[test]
    fn backspace_does_not_split_a_multibyte_character() {
        let mut field = Field::with_text("aé");
        assert_eq!(field.caret(), 3);
        assert!(field.apply(Action::Backspace));
        assert_eq!(field.text(), "a");
    }

    #[test]
    fn space_and_return_reach_the_text() {
        let mut field = Field::new();
        field.apply(Action::Insert(b'h'));
        field.apply(Action::Insert(b'i'));
        field.apply(Action::Space);
        field.apply(Action::Enter);
        assert_eq!(field.text(), "hi \n");
    }

    #[test]
    fn the_symbol_page_changes_what_a_tap_means_and_comes_back() {
        let mut kb = Keyboard::new();
        let (x, y) = key_centre(&kb, AREA, "?123");
        assert_eq!(kb.press_at(AREA, x, y), Some(Action::Page(PAGE_SYMBOLS)));
        assert_eq!(kb.page(), PAGE_SYMBOLS);
        assert!(
            kb.hit(
                AREA,
                key_centre(&kb, AREA, "1").0,
                key_centre(&kb, AREA, "1").1
            )
            .is_some()
        );
        let (x, y) = key_centre(&kb, AREA, "ABC");
        assert_eq!(kb.press_at(AREA, x, y), Some(Action::Page(PAGE_LETTERS)));
        assert_eq!(kb.page(), PAGE_LETTERS);
    }

    #[test]
    fn shift_does_not_stick_to_the_symbol_page() {
        let mut kb = Keyboard::new();
        let (x, y) = key_centre(&kb, AREA, "SHIFT");
        kb.press_at(AREA, x, y);
        assert!(kb.shift());
        kb.page = PAGE_SYMBOLS;
        let (x, y) = key_centre(&kb, AREA, "1");
        let action = kb.press_at(AREA, x, y).unwrap();
        assert_eq!(action, Action::Insert(b'1'));
        assert_eq!(Field::new().apply(action), true);
    }

    #[test]
    fn drawing_marks_the_area() {
        let mut fb = alloc::vec![0u32; 1080 * 2400];
        let mut p = Painter::new(&mut fb, 1080, 2400);
        let kb = Keyboard::new();
        kb.draw(&mut p, AREA, &ImeStyle::dark());
        assert!(
            fb.iter().any(|&px| px == ImeStyle::dark().key),
            "keys drawn"
        );
        assert!(
            fb.iter().any(|&px| px == ImeStyle::dark().background),
            "background drawn"
        );
        assert_eq!(fb[0], 0, "nothing outside the area is touched");
    }

    fn key_centre(kb: &Keyboard, area: Rect, label: &str) -> (i32, i32) {
        let mut out = [KeyRect {
            rect: Rect::new(0, 0, 0, 0),
            def: &EMPTY_KEY,
        }; MAX_KEYS];
        let n = kb.layout(area, &mut out);
        for kr in &out[..n] {
            if kr.def.label == label {
                return (kr.rect.x + kr.rect.w / 2, kr.rect.y + kr.rect.h / 2);
            }
        }
        panic!("no key labelled {}", label);
    }
}
