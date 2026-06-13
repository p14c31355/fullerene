//! InputContext — unified keyboard+mouse event queue.
//!
//! PS/2 events are polled here.  A bridge function
//! (`drain_into_event_context`) converts local events into
//! `resonance` types and pushes them into the kernel's
//! `EventContext`.
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use spin::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Other(u8),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    Shift,
    Ctrl,
    Alt,
    Meta,
    SuperLeft,
    SuperRight,
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
    Unknown(u32),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    MouseMove { x: i32, y: i32 },
    MouseDown(MouseButton),
    MouseUp(MouseButton),
    KeyDown(KeyCode),
    KeyUp(KeyCode),
}

const MAX_EVENTS: usize = 256;

pub struct InputContext {
    pub queue: VecDeque<InputEvent>,
    pub mouse_x: i16,
    pub mouse_y: i16,
    pub mouse_buttons: u8,
    prev_buttons: u8,
    sensitivity: i16,
}

impl InputContext {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            mouse_x: 512,
            mouse_y: 384,
            mouse_buttons: 0,
            prev_buttons: 0,
            sensitivity: 6,
        }
    }
    pub fn set_sensitivity(&mut self, s: i16) {
        self.sensitivity = s;
    }
    pub fn poll(&mut self) {
        let ps2 = nitrogen::ps2::mouse::consume_state();
        let (dx, dy, btn) = (
            ps2.get_x(),
            ps2.get_y(),
            nitrogen::ps2::mouse::mouse_buttons(),
        );
        let (ox, oy) = (self.mouse_x, self.mouse_y);
        self.mouse_x = self.mouse_x.wrapping_add(dx.wrapping_mul(self.sensitivity));
        self.mouse_y = self
            .mouse_y
            .wrapping_add(dy.wrapping_mul(self.sensitivity).wrapping_neg());
        self.mouse_buttons = btn;
        if ox != self.mouse_x || oy != self.mouse_y {
            self.push(InputEvent::MouseMove {
                x: self.mouse_x as i32,
                y: self.mouse_y as i32,
            });
        }
        let ch = btn ^ self.prev_buttons;
        if ch & 1 != 0 {
            self.push(if btn & 1 != 0 {
                InputEvent::MouseDown(MouseButton::Left)
            } else {
                InputEvent::MouseUp(MouseButton::Left)
            });
        }
        if ch & 2 != 0 {
            self.push(if btn & 2 != 0 {
                InputEvent::MouseDown(MouseButton::Right)
            } else {
                InputEvent::MouseUp(MouseButton::Right)
            });
        }
        if ch & 4 != 0 {
            self.push(if btn & 4 != 0 {
                InputEvent::MouseDown(MouseButton::Middle)
            } else {
                InputEvent::MouseUp(MouseButton::Middle)
            });
        }
        self.prev_buttons = btn;
        while nitrogen::ps2::keyboard::raw_key_available() {
            let (sc, pr) = match nitrogen::ps2::keyboard::pop_raw_key() {
                Some(k) => k,
                None => break,
            };
            self.push(if pr {
                InputEvent::KeyDown(scancode_to_keycode(sc))
            } else {
                InputEvent::KeyUp(scancode_to_keycode(sc))
            });
        }
    }
    fn push(&mut self, ev: InputEvent) {
        while self.queue.len() >= MAX_EVENTS {
            self.queue.pop_front();
        }
        self.queue.push_back(ev);
    }
    pub fn drain_events(&mut self) -> Vec<InputEvent> {
        self.queue.drain(..).collect()
    }
    pub fn has_events(&self) -> bool {
        !self.queue.is_empty()
    }
}

// ── Bridge to EventContext ─────────────────────────────────
/// Convert local InputEvent → resonance::Event and push into the
/// kernel-global EventContext.  Call after `poll()` each tick.
pub fn drain_into_event_context() {
    use resonance::{Event, InputEvent as ResInput, KeyCode as ResKey, MouseButton as ResBtn};

    let events = with_input_mut(|ctx| ctx.drain_events());
    let Some(events) = events else { return };

    super::event::with_event_mut(|ec| {
        for ev in events {
            let res_ev = match ev {
                InputEvent::MouseMove { x, y } => {
                    ResInput::MouseMove { x, y }
                }
                InputEvent::MouseDown(b) => {
                    ResInput::MouseDown(convert_btn(b))
                }
                InputEvent::MouseUp(b) => {
                    ResInput::MouseUp(convert_btn(b))
                }
                InputEvent::KeyDown(k) => {
                    ResInput::KeyDown(convert_key(k))
                }
                InputEvent::KeyUp(k) => {
                    ResInput::KeyUp(convert_key(k))
                }
            };
            ec.push(Event::Input(res_ev));
        }
    });
}

fn convert_btn(b: MouseButton) -> resonance::MouseButton {
    match b {
        MouseButton::Left => resonance::MouseButton::Left,
        MouseButton::Middle => resonance::MouseButton::Middle,
        MouseButton::Right => resonance::MouseButton::Right,
        MouseButton::Other(v) => resonance::MouseButton::Other(v),
    }
}

fn convert_key(k: KeyCode) -> resonance::KeyCode {
    use KeyCode::*;
    use resonance::KeyCode as R;
    match k {
        A => R::A, B => R::B, C => R::C, D => R::D, E => R::E,
        F => R::F, G => R::G, H => R::H, I => R::I, J => R::J,
        K => R::K, L => R::L, M => R::M, N => R::N, O => R::O,
        P => R::P, Q => R::Q, R => R::R, S => R::S, T => R::T,
        U => R::U, V => R::V, W => R::W, X => R::X, Y => R::Y,
        Z => R::Z,
        Digit0 => R::Digit0, Digit1 => R::Digit1, Digit2 => R::Digit2,
        Digit3 => R::Digit3, Digit4 => R::Digit4, Digit5 => R::Digit5,
        Digit6 => R::Digit6, Digit7 => R::Digit7, Digit8 => R::Digit8,
        Digit9 => R::Digit9,
        Shift => R::Shift, Ctrl => R::Ctrl, Alt => R::Alt, Meta => R::Meta,
        SuperLeft => R::SuperLeft, SuperRight => R::SuperRight,
        Enter => R::Enter, Tab => R::Tab, Space => R::Space,
        Backspace => R::Backspace, Escape => R::Escape,
        Up => R::Up, Down => R::Down, Left => R::Left, Right => R::Right,
        Home => R::Home, End => R::End,
        PageUp => R::PageUp, PageDown => R::PageDown,
        F1 => R::F1, F2 => R::F2, F3 => R::F3, F4 => R::F4,
        F5 => R::F5, F6 => R::F6, F7 => R::F7, F8 => R::F8,
        F9 => R::F9, F10 => R::F10, F11 => R::F11, F12 => R::F12,
        Unknown(v) => R::Unknown(v),
    }
}

fn scancode_to_keycode(sc: u8) -> KeyCode {
    use KeyCode::*;
    const EXT: [Option<KeyCode>; 128] = {
        let mut t = [None; 128];
        t[0x1D] = Some(Ctrl);
        t[0x38] = Some(Alt);
        t[0x5B] = Some(SuperLeft);
        t[0x5C] = Some(SuperRight);
        t
    };
    const BASE: [KeyCode; 128] = {
        let mut t = [Unknown(0); 128];
        let mut i = 0;
        while i < 128 {
            t[i] = Unknown(i as u32);
            i += 1;
        }
        t[0x01] = Escape;
        t[0x02] = Digit1;
        t[0x03] = Digit2;
        t[0x04] = Digit3;
        t[0x05] = Digit4;
        t[0x06] = Digit5;
        t[0x07] = Digit6;
        t[0x08] = Digit7;
        t[0x09] = Digit8;
        t[0x0A] = Digit9;
        t[0x0B] = Digit0;
        t[0x0E] = Backspace;
        t[0x0F] = Tab;
        t[0x10] = Q;
        t[0x11] = W;
        t[0x12] = E;
        t[0x13] = R;
        t[0x14] = T;
        t[0x15] = Y;
        t[0x16] = U;
        t[0x17] = I;
        t[0x18] = O;
        t[0x19] = P;
        t[0x1C] = Enter;
        t[0x1D] = Ctrl;
        t[0x1E] = A;
        t[0x1F] = S;
        t[0x20] = D;
        t[0x21] = F;
        t[0x22] = G;
        t[0x23] = H;
        t[0x24] = J;
        t[0x25] = K;
        t[0x26] = L;
        t[0x2A] = Shift;
        t[0x2C] = Z;
        t[0x2D] = X;
        t[0x2E] = C;
        t[0x2F] = V;
        t[0x30] = B;
        t[0x31] = N;
        t[0x32] = M;
        t[0x36] = Shift;
        t[0x38] = Alt;
        t[0x39] = Space;
        t[0x3B] = F1;
        t[0x3C] = F2;
        t[0x3D] = F3;
        t[0x3E] = F4;
        t[0x3F] = F5;
        t[0x40] = F6;
        t[0x41] = F7;
        t[0x42] = F8;
        t[0x43] = F9;
        t[0x44] = F10;
        t[0x47] = Home;
        t[0x48] = Up;
        t[0x49] = PageUp;
        t[0x4B] = Left;
        t[0x4D] = Right;
        t[0x4F] = End;
        t[0x50] = Down;
        t[0x51] = PageDown;
        t[0x57] = F11;
        t[0x58] = F12;
        t
    };
    let b = sc & 0x7F;
    if sc & 0x80 != 0 {
        EXT[b as usize].unwrap_or(BASE[b as usize])
    } else {
        BASE[b as usize]
    }
}

static INPUT_CTX: Mutex<Option<InputContext>> = Mutex::new(None);
pub fn init_input() {
    *INPUT_CTX.lock() = Some(InputContext::new());
}
pub fn get_input() -> &'static Mutex<Option<InputContext>> {
    &INPUT_CTX
}
pub fn with_input_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut InputContext) -> R,
{
    INPUT_CTX.lock().as_mut().map(f)
}
pub fn with_input<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&InputContext) -> R,
{
    INPUT_CTX.lock().as_ref().map(f)
}