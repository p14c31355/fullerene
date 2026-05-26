//! Keyboard layout / keymap abstraction
//!
//! Converts scancodes to symbolic key codes using layout tables.
//! Supports modifier-aware mapping (Shift, Ctrl, Alt) and multiple layouts
//! (US QWERTY, Japanese 106-key).

/// Symbolic key code (independent of any particular event system).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyCode {
    // Alphanumeric
    A, B, C, D, E, F, G, H, I, J, K, L, M,
    N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
    Digit0, Digit1, Digit2, Digit3, Digit4,
    Digit5, Digit6, Digit7, Digit8, Digit9,

    // Modifiers
    LShift, RShift,
    LCtrl, RCtrl,
    LAlt, RAlt,

    // Navigation
    Enter, Tab, Space, Backspace, Escape,
    Up, Down, Left, Right,
    Home, End, PageUp, PageDown,

    // Function keys
    F1, F2, F3, F4, F5, F6,
    F7, F8, F9, F10, F11, F12,

    // Japanese layout special keys
    Kana,  // 半角/全角 or ひらがな
    Muhenkan, // 無変換
    Henkan,   // 変換
    Katakana, // カタカナ

    /// Catch-all for unhandled keys.
    Unknown(u8),
}

/// Scancode set — currently only PS/2 Set 1 is implemented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScancodeSet {
    /// PS/2 scancode set 1 (XT).
    Set1,
}

/// Keyboard layout identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// US QWERTY (104-key).
    UsQwerty,
    /// Japanese JIS 106-key.
    JapaneseJis,
}

/// Modifier flags bitmask.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub bits: u8,
}

impl Modifiers {
    pub const SHIFT: u8 = 0x01;
    pub const CTRL: u8 = 0x02;
    pub const ALT: u8 = 0x04;
    pub const META: u8 = 0x08;

    pub fn shift(&self) -> bool { self.bits & Self::SHIFT != 0 }
    pub fn ctrl(&self) -> bool { self.bits & Self::CTRL != 0 }
    pub fn alt(&self) -> bool { self.bits & Self::ALT != 0 }
    pub fn meta(&self) -> bool { self.bits & Self::META != 0 }
    pub fn set(&mut self, mask: u8, on: bool) {
        if on { self.bits |= mask; } else { self.bits &= !mask; }
    }
}

/// Keymap — converts scancodes to [`KeyCode`] based on layout.
pub struct Keymap {
    layout: Layout,
    scancode_set: ScancodeSet,
}

impl Keymap {
    /// Create a new keymap with the given layout.
    pub fn new(layout: Layout) -> Self {
        Self { layout, scancode_set: ScancodeSet::Set1 }
    }

    /// Change the layout at runtime.
    pub fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
    }

    /// Get the current layout.
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Convert a scancode + modifiers into a [`KeyCode`].
    ///
    /// Returns `None` for scancodes that don't produce a known key
    /// in the current layout.
    pub fn to_keycode(&self, scancode: u8, mods: &Modifiers) -> Option<KeyCode> {
        let shifted = mods.shift();
        match self.layout {
            Layout::UsQwerty => Self::us_set1_keycode(scancode, shifted),
            Layout::JapaneseJis => Self::jp_set1_keycode(scancode, shifted),
        }
    }

    /// Convert a [`KeyCode`] + modifiers to an ASCII byte (if printable).
    pub fn to_ascii(key: KeyCode, mods: &Modifiers) -> Option<u8> {
        let upper = mods.shift();
        let ctrl = mods.ctrl();
        match key {
            KeyCode::Enter => Some(b'\n'),
            KeyCode::Space => Some(b' '),
            KeyCode::Backspace => Some(0x08),
            KeyCode::Tab => Some(b'\t'),
            KeyCode::Escape => Some(0x1B),
            KeyCode::A => if ctrl { Some(0x01) } else { Some(if upper { b'A' } else { b'a' }) },
            KeyCode::B => if ctrl { Some(0x02) } else { Some(if upper { b'B' } else { b'b' }) },
            KeyCode::C => if ctrl { Some(0x03) } else { Some(if upper { b'C' } else { b'c' }) },
            KeyCode::D => if ctrl { Some(0x04) } else { Some(if upper { b'D' } else { b'd' }) },
            KeyCode::E => if ctrl { Some(0x05) } else { Some(if upper { b'E' } else { b'e' }) },
            KeyCode::F => if ctrl { Some(0x06) } else { Some(if upper { b'F' } else { b'f' }) },
            KeyCode::G => if ctrl { Some(0x07) } else { Some(if upper { b'G' } else { b'g' }) },
            KeyCode::H => if ctrl { Some(0x08) } else { Some(if upper { b'H' } else { b'h' }) },
            KeyCode::I => if ctrl { Some(0x09) } else { Some(if upper { b'I' } else { b'i' }) },
            KeyCode::J => if ctrl { Some(0x0A) } else { Some(if upper { b'J' } else { b'j' }) },
            KeyCode::K => if ctrl { Some(0x0B) } else { Some(if upper { b'K' } else { b'k' }) },
            KeyCode::L => if ctrl { Some(0x0C) } else { Some(if upper { b'L' } else { b'l' }) },
            KeyCode::M => if ctrl { Some(0x0D) } else { Some(if upper { b'M' } else { b'm' }) },
            KeyCode::N => if ctrl { Some(0x0E) } else { Some(if upper { b'N' } else { b'n' }) },
            KeyCode::O => if ctrl { Some(0x0F) } else { Some(if upper { b'O' } else { b'o' }) },
            KeyCode::P => if ctrl { Some(0x10) } else { Some(if upper { b'P' } else { b'p' }) },
            KeyCode::Q => if ctrl { Some(0x11) } else { Some(if upper { b'Q' } else { b'q' }) },
            KeyCode::R => if ctrl { Some(0x12) } else { Some(if upper { b'R' } else { b'r' }) },
            KeyCode::S => if ctrl { Some(0x13) } else { Some(if upper { b'S' } else { b's' }) },
            KeyCode::T => if ctrl { Some(0x14) } else { Some(if upper { b'T' } else { b't' }) },
            KeyCode::U => if ctrl { Some(0x15) } else { Some(if upper { b'U' } else { b'u' }) },
            KeyCode::V => if ctrl { Some(0x16) } else { Some(if upper { b'V' } else { b'v' }) },
            KeyCode::W => if ctrl { Some(0x17) } else { Some(if upper { b'W' } else { b'w' }) },
            KeyCode::X => if ctrl { Some(0x18) } else { Some(if upper { b'X' } else { b'x' }) },
            KeyCode::Y => if ctrl { Some(0x19) } else { Some(if upper { b'Y' } else { b'y' }) },
            KeyCode::Z => if ctrl { Some(0x1A) } else { Some(if upper { b'Z' } else { b'z' }) },
            KeyCode::Digit0 => Some(if upper { b')' } else { b'0' }),
            KeyCode::Digit1 => Some(if upper { b'!' } else { b'1' }),
            KeyCode::Digit2 => Some(if upper { b'@' } else { b'2' }),
            KeyCode::Digit3 => Some(if upper { b'#' } else { b'3' }),
            KeyCode::Digit4 => Some(if upper { b'$' } else { b'4' }),
            KeyCode::Digit5 => Some(if upper { b'%' } else { b'5' }),
            KeyCode::Digit6 => Some(if upper { b'^' } else { b'6' }),
            KeyCode::Digit7 => Some(if upper { b'&' } else { b'7' }),
            KeyCode::Digit8 => Some(if upper { b'*' } else { b'8' }),
            KeyCode::Digit9 => Some(if upper { b'(' } else { b'9' }),
            _ => None,
        }
    }

    // ── US QWERTY Set 1 mapping ──────────────────────────

    fn us_set1_keycode(scancode: u8, _shifted: bool) -> Option<KeyCode> {
        match scancode {
            0x01 => Some(KeyCode::Escape),
            0x02 => Some(KeyCode::Digit1),
            0x03 => Some(KeyCode::Digit2),
            0x04 => Some(KeyCode::Digit3),
            0x05 => Some(KeyCode::Digit4),
            0x06 => Some(KeyCode::Digit5),
            0x07 => Some(KeyCode::Digit6),
            0x08 => Some(KeyCode::Digit7),
            0x09 => Some(KeyCode::Digit8),
            0x0A => Some(KeyCode::Digit9),
            0x0B => Some(KeyCode::Digit0),
            0x0C => None, // minus / underscore
            0x0D => None, // equals / plus
            0x0E => Some(KeyCode::Backspace),
            0x0F => Some(KeyCode::Tab),
            0x10 => Some(KeyCode::Q),
            0x11 => Some(KeyCode::W),
            0x12 => Some(KeyCode::E),
            0x13 => Some(KeyCode::R),
            0x14 => Some(KeyCode::T),
            0x15 => Some(KeyCode::Y),
            0x16 => Some(KeyCode::U),
            0x17 => Some(KeyCode::I),
            0x18 => Some(KeyCode::O),
            0x19 => Some(KeyCode::P),
            0x1A => None, // left bracket
            0x1B => None, // right bracket
            0x1C => Some(KeyCode::Enter),
            0x1D => Some(KeyCode::LCtrl),
            0x1E => Some(KeyCode::A),
            0x1F => Some(KeyCode::S),
            0x20 => Some(KeyCode::D),
            0x21 => Some(KeyCode::F),
            0x22 => Some(KeyCode::G),
            0x23 => Some(KeyCode::H),
            0x24 => Some(KeyCode::J),
            0x25 => Some(KeyCode::K),
            0x26 => Some(KeyCode::L),
            0x27 => None, // semicolon
            0x28 => None, // apostrophe
            0x29 => None, // backtick
            0x2A => Some(KeyCode::LShift),
            0x2B => None, // backslash
            0x2C => Some(KeyCode::Z),
            0x2D => Some(KeyCode::X),
            0x2E => Some(KeyCode::C),
            0x2F => Some(KeyCode::V),
            0x30 => Some(KeyCode::B),
            0x31 => Some(KeyCode::N),
            0x32 => Some(KeyCode::M),
            0x33 => None, // comma
            0x34 => None, // period
            0x35 => None, // slash
            0x36 => Some(KeyCode::RShift),
            0x37 => None, // keypad *
            0x38 => Some(KeyCode::LAlt),
            0x39 => Some(KeyCode::Space),
            0x3A => None, // Caps Lock
            0x3B => Some(KeyCode::F1),
            0x3C => Some(KeyCode::F2),
            0x3D => Some(KeyCode::F3),
            0x3E => Some(KeyCode::F4),
            0x3F => Some(KeyCode::F5),
            0x40 => Some(KeyCode::F6),
            0x41 => Some(KeyCode::F7),
            0x42 => Some(KeyCode::F8),
            0x43 => Some(KeyCode::F9),
            0x44 => Some(KeyCode::F10),
            0x45 => None, // Num Lock
            0x46 => None, // Scroll Lock
            0x47 => Some(KeyCode::Home),
            0x48 => Some(KeyCode::Up),
            0x49 => Some(KeyCode::PageUp),
            0x4B => Some(KeyCode::Left),
            0x4D => Some(KeyCode::Right),
            0x4F => Some(KeyCode::End),
            0x50 => Some(KeyCode::Down),
            0x51 => Some(KeyCode::PageDown),
            0x52 => None, // Insert
            0x53 => None, // Delete
            0x57 => Some(KeyCode::F11),
            0x58 => Some(KeyCode::F12),
            _ => None,
        }
    }

    // ── Japanese JIS 106-key Set 1 mapping (stub) ────────

    fn jp_set1_keycode(scancode: u8, _shifted: bool) -> Option<KeyCode> {
        match scancode {
            0x29 => Some(KeyCode::Kana),    // 半角/全角
            0x73 => Some(KeyCode::Muhenkan), // 無変換
            0x7B => Some(KeyCode::Henkan),   // 変換
            0x79 => Some(KeyCode::Katakana), // ひらがな/カタカナ
            _ => Self::us_set1_keycode(scancode, _shifted),
        }
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Self::new(Layout::UsQwerty)
    }
}