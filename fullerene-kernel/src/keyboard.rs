//! Keyboard input driver for Fullerene OS
//!
//! PS/2 keyboard scancode-to-ASCII conversion with input buffering.
//! Uses manual scancode set 1 tables for reliable no_std operation.

use alloc::collections::VecDeque;
use alloc::string::String;
use petroleum::declare_init;
use spin::Mutex;

/// Keyboard input buffer
static INPUT_BUFFER: Mutex<VecDeque<u8>> = Mutex::new(VecDeque::new());
static INPUT_STRING_BUFFER: Mutex<String> = Mutex::new(String::new());

/// Keyboard modifiers state
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyboardModifiers {
    pub lshift: bool,
    pub rshift: bool,
    pub lctrl: bool,
    pub rctrl: bool,
    pub lalt: bool,
    pub ralt: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
    pub scroll_lock: bool,
}

static MODIFIERS: Mutex<KeyboardModifiers> = Mutex::new(KeyboardModifiers {
    lshift: false,
    rshift: false,
    lctrl: false,
    rctrl: false,
    lalt: false,
    ralt: false,
    caps_lock: false,
    num_lock: false,
    scroll_lock: false,
});

/// Extended scancode flag
static EXTENDED_SCANCODE: Mutex<bool> = Mutex::new(false);

// Scancode set 1 to ASCII lookup tables
const NUMBERS_BASE: &[u8] = b"1234567890";
const NUMBERS_SHIFT: &[u8] = b"!@#$%^&*()";
const QWERTY_BASE: &[u8] = b"qwertyuiop";
const ASDF_BASE: &[u8] = b"asdfghjkl";
const ZXCV_BASE: &[u8] = b"zxcvbnm";

// Punctuation mappings (scancode, base, shifted)
const PUNCTUATION: &[(u8, u8, u8)] = &[
    (0x0C, b'-', b'_'),
    (0x0D, b'=', b'+'),
    (0x1A, b'[', b'{'),
    (0x1B, b']', b'}'),
    (0x27, b';', b':'),
    (0x28, b'\'', b'"'),
    (0x29, b'`', b'~'),
    (0x2B, b'\\', b'|'),
    (0x33, b',', b'<'),
    (0x34, b'.', b'>'),
    (0x35, b'/', b'?'),
];

// Special keys (scancode, ascii)
const SPECIAL_KEYS: &[(u8, u8)] = &[
    (0x1C, b'\n'), // Enter
    (0x0E, 0x08),  // Backspace
    (0x0F, b'\t'), // Tab
    (0x01, 27),    // Escape
    (0x39, b' '),  // Space
];

/// Apply case/ctrl modifications to alphabetic character
fn process_alphabetic(base: u8, modifiers: &KeyboardModifiers) -> u8 {
    let shift_pressed = modifiers.lshift || modifiers.rshift;
    let caps_lock = modifiers.caps_lock;
    let ctrl_pressed = modifiers.lctrl || modifiers.rctrl;

    let mut ch = base;
    if shift_pressed ^ caps_lock {
        ch = ch.to_ascii_uppercase();
    }
    if ctrl_pressed {
        ch &= 0x1F;
    }
    ch
}

/// Convert scancode set 1 to ASCII using lookup tables
fn scancode_to_ascii(scancode: u8, modifiers: &KeyboardModifiers) -> Option<u8> {
    let shift_pressed = modifiers.lshift || modifiers.rshift;

    match scancode {
        0x02..=0x0B => {
            let index = (scancode - 0x02) as usize;
            Some(if shift_pressed { NUMBERS_SHIFT[index] } else { NUMBERS_BASE[index] })
        }
        0x10..=0x19 => {
            let index = (scancode - 0x10) as usize;
            Some(process_alphabetic(QWERTY_BASE[index], modifiers))
        }
        0x1E..=0x26 => {
            let index = (scancode - 0x1E) as usize;
            Some(process_alphabetic(ASDF_BASE[index], modifiers))
        }
        0x2C..=0x32 => {
            let index = (scancode - 0x2C) as usize;
            Some(process_alphabetic(ZXCV_BASE[index], modifiers))
        }
        0x0C | 0x0D | 0x1A | 0x1B | 0x27 | 0x28 | 0x29 | 0x2B | 0x33 | 0x34 | 0x35 => {
            PUNCTUATION.iter()
                .find(|&&(code, _, _)| code == scancode)
                .map(|&(_, base, shifted)| if shift_pressed { shifted } else { base })
        }
        _ => SPECIAL_KEYS.iter()
            .find(|&&(code, _)| code == scancode)
            .map(|&(_, ascii)| ascii),
    }
}

/// Handle keyboard interrupt with scancode
pub fn handle_keyboard_scancode(scancode: u8) {
    let mut extended_flag = EXTENDED_SCANCODE.lock();
    if scancode == 0xE0 {
        *extended_flag = true;
        return;
    }

    let is_extended = *extended_flag;
    *extended_flag = false;
    drop(extended_flag);

    let mut modifiers = MODIFIERS.lock();

    if is_extended {
        if scancode & 0x80 != 0 {
            handle_extended_key_release(scancode & 0x7F, &mut modifiers);
        } else {
            handle_extended_key_press(scancode, &mut modifiers);
        }
    } else if scancode & 0x80 != 0 {
        handle_key_release(scancode & 0x7F, &mut modifiers);
    } else {
        handle_key_press(scancode, &mut modifiers);
    }
}

fn handle_key_press(scancode: u8, modifiers: &mut KeyboardModifiers) {
    match scancode {
        0x2A => modifiers.lshift = true,
        0x36 => modifiers.rshift = true,
        0x1D => modifiers.lctrl = true,
        0x38 => modifiers.lalt = true,
        0x3A => modifiers.caps_lock = !modifiers.caps_lock,
        0x45 => modifiers.num_lock = !modifiers.num_lock,
        0x46 => modifiers.scroll_lock = !modifiers.scroll_lock,
        _ => {
            if let Some(ascii) = scancode_to_ascii(scancode, modifiers) {
                let mut buffer = INPUT_BUFFER.lock();
                if buffer.len() < 256 {
                    buffer.push_back(ascii);
                }

                let mut str_buffer = INPUT_STRING_BUFFER.lock();
                if ascii == 0x08 {
                    str_buffer.pop();
                } else {
                    str_buffer.push(ascii as char);
                }
            }
        }
    }
}

fn handle_key_release(scancode: u8, modifiers: &mut KeyboardModifiers) {
    match scancode {
        0x2A => modifiers.lshift = false,
        0x36 => modifiers.rshift = false,
        0x1D => modifiers.lctrl = false,
        0x38 => modifiers.lalt = false,
        _ => {}
    }
}

fn handle_extended_key_press(scancode: u8, modifiers: &mut KeyboardModifiers) {
    match scancode {
        0x1D => modifiers.rctrl = true,
        0x38 => modifiers.ralt = true,
        _ => {}
    }
}

fn handle_extended_key_release(scancode: u8, modifiers: &mut KeyboardModifiers) {
    match scancode {
        0x1D => modifiers.rctrl = false,
        0x38 => modifiers.ralt = false,
        _ => {}
    }
}

/// Read a character from keyboard (non-blocking)
pub fn read_char() -> Option<u8> {
    INPUT_BUFFER.lock().pop_front()
}

/// Drain line buffer (non-blocking)
pub fn drain_line_buffer(buffer: &mut [u8]) -> usize {
    let mut str_buffer = INPUT_STRING_BUFFER.lock();
    let str_bytes = str_buffer.as_bytes();
    let copy_len = str_bytes.len().min(buffer.len());
    buffer[..copy_len].copy_from_slice(&str_bytes[..copy_len]);
    str_buffer.clear();
    copy_len
}

/// Check if input is available
pub fn input_available() -> bool {
    !INPUT_BUFFER.lock().is_empty()
}

/// Flush input buffers
pub fn flush_input() {
    INPUT_BUFFER.lock().clear();
    INPUT_STRING_BUFFER.lock().clear();
}

/// Get keyboard modifier status
pub fn get_keyboard_status() -> KeyboardModifiers {
    *MODIFIERS.lock()
}

/// Initialize keyboard driver
pub fn init() {
    flush_input();
    declare_init!("Keyboard input driver");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scancode_conversion() {
        let mods = KeyboardModifiers::default();
        assert_eq!(scancode_to_ascii(0x1E, &mods), Some(b'a'));
        assert_eq!(scancode_to_ascii(0x10, &mods), Some(b'q'));
        assert_eq!(scancode_to_ascii(0x39, &mods), Some(b' '));
        assert_eq!(scancode_to_ascii(0x1C, &mods), Some(b'\n'));
        assert_eq!(scancode_to_ascii(0xFF, &mods), None);
    }

    #[test]
    fn test_buffer_operations() {
        init();
        assert_eq!(read_char(), None);
        INPUT_BUFFER.lock().push_back(b't');
        assert!(input_available());
        assert_eq!(read_char(), Some(b't'));
        assert_eq!(read_char(), None);
    }
}