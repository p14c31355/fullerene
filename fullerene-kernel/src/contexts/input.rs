//! InputContext — unified keyboard+mouse event queue.
use alloc::collections::VecDeque;
use alloc::vec::Vec;
pub use resonance::{InputEvent, KeyCode, MouseButton};

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
    /// Read sensitivity from SettingsContext if available, otherwise use the
    /// locally-configured value.
    pub fn apply_settings_sensitivity(&mut self) {
        if let Some(val) = super::kernel::with_kernel(|k| k.settings.mouse.sensitivity_raw()) {
            self.sensitivity = val;
        }
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

pub fn drain_into_event_context() {
    let has_event_ctx = super::event::with_event_mut(|_| ()).is_some();
    if !has_event_ctx {
        return;
    }
    let events = with_input_mut(|ctx| ctx.drain_events());
    let Some(events) = events else { return };
    super::event::with_event_mut(|ec| {
        for ev in events {
            ec.push(resonance::Event::Input(ev));
        }
    });
}

fn scancode_to_keycode(sc: u8) -> KeyCode {
    use KeyCode::*;
    const BASE: [KeyCode; 128] = {
        let mut t = [Unknown(0); 128];
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
    BASE[b as usize]
}

crate::define_context!(InputContext, input, INPUT_CTX);
