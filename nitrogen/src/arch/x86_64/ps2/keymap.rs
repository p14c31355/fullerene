//! Keyboard layout / keymap abstraction.
//!
//! Converts scancodes to symbolic key codes and back to ASCII,
//! supporting US QWERTY and Japanese JIS 106-key layouts.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyCode {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    LShift,
    RShift,
    LCtrl,
    RCtrl,
    LAlt,
    RAlt,
    LSuper,
    RSuper,
    Enter,
    Tab,
    Space,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    Kana,
    Muhenkan,
    Henkan,
    Katakana,
    Unknown(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScancodeSet {
    Set1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    UsQwerty,
    JapaneseJis,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub bits: u8,
}

impl Modifiers {
    pub const SHIFT: u8 = 0x01;
    pub const CTRL: u8 = 0x02;
    pub const ALT: u8 = 0x04;
    pub const META: u8 = 0x08;
    pub fn shift(&self) -> bool {
        self.bits & Self::SHIFT != 0
    }
    pub fn ctrl(&self) -> bool {
        self.bits & Self::CTRL != 0
    }
    pub fn alt(&self) -> bool {
        self.bits & Self::ALT != 0
    }
    pub fn meta(&self) -> bool {
        self.bits & Self::META != 0
    }
    pub fn set(&mut self, mask: u8, on: bool) {
        if on {
            self.bits |= mask;
        } else {
            self.bits &= !mask;
        }
    }
}

pub struct Keymap {
    layout: Layout,
    #[allow(dead_code)]
    scancode_set: ScancodeSet,
}

impl Keymap {
    pub fn new(layout: Layout) -> Self {
        Self {
            layout,
            scancode_set: ScancodeSet::Set1,
        }
    }
    pub fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
    }
    pub fn layout(&self) -> Layout {
        self.layout
    }

    pub fn to_keycode(&self, scancode: u8, mods: &Modifiers) -> Option<KeyCode> {
        let _shifted = mods.shift();
        match self.layout {
            Layout::UsQwerty => us_set1_keycode(scancode),
            Layout::JapaneseJis => jp_set1_keycode(scancode),
        }
    }

    pub fn to_ascii(key: KeyCode, mods: &Modifiers) -> Option<u8> {
        let upper = mods.shift();
        let ctrl = mods.ctrl();
        fn alpha(ctrl: bool, upper: bool, ctrl_byte: u8, uc: u8, lc: u8) -> Option<u8> {
            Some(if ctrl {
                ctrl_byte
            } else if upper {
                uc
            } else {
                lc
            })
        }
        use KeyCode::*;
        match key {
            Enter => Some(b'\n'),
            Space => Some(b' '),
            Backspace => Some(0x08),
            Tab => Some(b'\t'),
            Escape => Some(0x1B),
            A => alpha(ctrl, upper, 0x01, b'A', b'a'),
            B => alpha(ctrl, upper, 0x02, b'B', b'b'),
            C => alpha(ctrl, upper, 0x03, b'C', b'c'),
            D => alpha(ctrl, upper, 0x04, b'D', b'd'),
            E => alpha(ctrl, upper, 0x05, b'E', b'e'),
            F => alpha(ctrl, upper, 0x06, b'F', b'f'),
            G => alpha(ctrl, upper, 0x07, b'G', b'g'),
            H => alpha(ctrl, upper, 0x08, b'H', b'h'),
            I => alpha(ctrl, upper, 0x09, b'I', b'i'),
            J => alpha(ctrl, upper, 0x0A, b'J', b'j'),
            K => alpha(ctrl, upper, 0x0B, b'K', b'k'),
            L => alpha(ctrl, upper, 0x0C, b'L', b'l'),
            M => alpha(ctrl, upper, 0x0D, b'M', b'm'),
            N => alpha(ctrl, upper, 0x0E, b'N', b'n'),
            O => alpha(ctrl, upper, 0x0F, b'O', b'o'),
            P => alpha(ctrl, upper, 0x10, b'P', b'p'),
            Q => alpha(ctrl, upper, 0x11, b'Q', b'q'),
            R => alpha(ctrl, upper, 0x12, b'R', b'r'),
            S => alpha(ctrl, upper, 0x13, b'S', b's'),
            T => alpha(ctrl, upper, 0x14, b'T', b't'),
            U => alpha(ctrl, upper, 0x15, b'U', b'u'),
            V => alpha(ctrl, upper, 0x16, b'V', b'v'),
            W => alpha(ctrl, upper, 0x17, b'W', b'w'),
            X => alpha(ctrl, upper, 0x18, b'X', b'x'),
            Y => alpha(ctrl, upper, 0x19, b'Y', b'y'),
            Z => alpha(ctrl, upper, 0x1A, b'Z', b'z'),
            Digit0 => Some(if upper { b')' } else { b'0' }),
            Digit1 => Some(if upper { b'!' } else { b'1' }),
            Digit2 => Some(if upper { b'@' } else { b'2' }),
            Digit3 => Some(if upper { b'#' } else { b'3' }),
            Digit4 => Some(if upper { b'$' } else { b'4' }),
            Digit5 => Some(if upper { b'%' } else { b'5' }),
            Digit6 => Some(if upper { b'^' } else { b'6' }),
            Digit7 => Some(if upper { b'&' } else { b'7' }),
            Digit8 => Some(if upper { b'*' } else { b'8' }),
            Digit9 => Some(if upper { b'(' } else { b'9' }),
            _ => None,
        }
    }
}

fn us_set1_keycode(scancode: u8) -> Option<KeyCode> {
    use KeyCode::*;
    match scancode {
        0x01 => Some(Escape),
        0x02 => Some(Digit1),
        0x03 => Some(Digit2),
        0x04 => Some(Digit3),
        0x05 => Some(Digit4),
        0x06 => Some(Digit5),
        0x07 => Some(Digit6),
        0x08 => Some(Digit7),
        0x09 => Some(Digit8),
        0x0A => Some(Digit9),
        0x0B => Some(Digit0),
        0x0E => Some(Backspace),
        0x0F => Some(Tab),
        0x10 => Some(Q),
        0x11 => Some(W),
        0x12 => Some(E),
        0x13 => Some(R),
        0x14 => Some(T),
        0x15 => Some(Y),
        0x16 => Some(U),
        0x17 => Some(I),
        0x18 => Some(O),
        0x19 => Some(P),
        0x1C => Some(Enter),
        0x1D => Some(LCtrl),
        0x1E => Some(A),
        0x1F => Some(S),
        0x20 => Some(D),
        0x21 => Some(F),
        0x22 => Some(G),
        0x23 => Some(H),
        0x24 => Some(J),
        0x25 => Some(K),
        0x26 => Some(L),
        0x2A => Some(LShift),
        0x2C => Some(Z),
        0x2D => Some(X),
        0x2E => Some(C),
        0x2F => Some(V),
        0x30 => Some(B),
        0x31 => Some(N),
        0x32 => Some(M),
        0x36 => Some(RShift),
        0x38 => Some(LAlt),
        0x39 => Some(Space),
        0x3B => Some(F1),
        0x3C => Some(F2),
        0x3D => Some(F3),
        0x3E => Some(F4),
        0x3F => Some(F5),
        0x40 => Some(F6),
        0x41 => Some(F7),
        0x42 => Some(F8),
        0x43 => Some(F9),
        0x44 => Some(F10),
        0x47 => Some(Home),
        0x48 => Some(Up),
        0x49 => Some(PageUp),
        0x4B => Some(Left),
        0x4D => Some(Right),
        0x4F => Some(End),
        0x50 => Some(Down),
        0x51 => Some(PageDown),
        0x57 => Some(F11),
        0x58 => Some(F12),
        0x5B => Some(LSuper),
        0x5C => Some(RSuper),
        _ => None,
    }
}

fn jp_set1_keycode(scancode: u8) -> Option<KeyCode> {
    match scancode {
        0x29 => Some(KeyCode::Kana),
        0x73 => Some(KeyCode::Muhenkan),
        0x7B => Some(KeyCode::Henkan),
        0x79 => Some(KeyCode::Katakana),
        _ => us_set1_keycode(scancode),
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Self::new(Layout::UsQwerty)
    }
}
