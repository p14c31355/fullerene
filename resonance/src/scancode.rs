use crate::KeyCode::*;
use crate::KeyCode;

pub const EXT: [Option<KeyCode>; 128] = {
    let mut t = [None; 128];
    t[0x1D] = Some(Ctrl);
    t[0x38] = Some(Alt);
    t[0x5B] = Some(SuperLeft);
    t[0x5C] = Some(SuperRight);
    t
};

pub const BASE: [KeyCode; 128] = {
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

pub fn from_scancode(scancode: u8) -> KeyCode {
    let base = (scancode & 0x7F) as usize;
    if scancode & 0x80 != 0 {
        EXT[base].unwrap_or(BASE[base])
    } else {
        BASE[base]
    }
}
