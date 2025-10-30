//! Keyboard input driver for Fullerene OS
//!
//! This module provides keyboard input functionality including:
//! - PS/2 keyboard protocol handling using pc-keyboard crate
//! - Scan code to ASCII conversion
//! - Input buffer management
//! - Blocking/non-blocking input

use alloc::collections::VecDeque;
use alloc::string::String;
use pc_keyboard::{Keyboard, ScancodeSet1, layouts};
use petroleum::declare_init;
use spin::Mutex;

// Using pc-keyboard for scan code handling
static KEYBOARD: Mutex<Option<Keyboard<layouts::Us104Key, ScancodeSet1>>> = Mutex::new(None);

/// Keyboard input buffer
static INPUT_BUFFER: Mutex<VecDeque<u8>> = Mutex::new(VecDeque::new());
static INPUT_STRING_BUFFER: Mutex<String> = Mutex::new(String::new());

/// Keyboard modifiers (shift, ctrl, alt, etc.) - simplified since pc-keyboard handles most
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

/// Flag for extended scancode handling
static EXTENDED_SCANCODE: Mutex<bool> = Mutex::new(false);

// Key mapping tables for scancode to ASCII conversion
const NUMBERS_BASE: &[u8] = b"1234567890";
const NUMBERS_SHIFT: &[u8] = b"!@#$%^&*()";

// QWERTY row characters
const QWERTY_BASE: &[u8] = b"qwertyuiop";
const ASDF_BASE: &[u8] = b"asdfghjkl";
const ZXCV_BASE: &[u8] = b"zxcvbnm";

// Punctuation mappings (scancode, base, shifted)
const PUNCTUATION: &[(u8, u8, u8)] = &[
    (0x0C, b'-', b')'),
    (0x0D, b'=', b'='),
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

/// Helper function to apply case and ctrl modifications to alphabetic characters
fn process_alphabetic(scancode: u8, base: u8, modifiers: &KeyboardModifiers) -> u8 {
    let shift_pressed = modifiers.lshift || modifiers.rshift;
    let caps_lock = modifiers.caps_lock;
    let ctrl_pressed = modifiers.lctrl || modifiers.rctrl;

    let mut ch = base;
    if shift_pressed ^ caps_lock {
        ch = ch.to_ascii_uppercase();
    }
    if ctrl_pressed {
        ch &= 0x1F; // Ctrl modifies ASCII
    }
    ch
}

/// Scancode set 1 to ASCII conversion using lookup tables
fn scancode_to_ascii(scancode: u8, modifiers: &KeyboardModifiers) -> Option<u8> {
    let shift_pressed = modifiers.lshift || modifiers.rshift;
    let ctrl_pressed = modifiers.lctrl || modifiers.rctrl;

    match scancode {
        0x02..=0x0B => {
            // Numbers 1-0
            let index = (scancode - 0x02) as usize;
            let chars = if shift_pressed {
                NUMBERS_SHIFT
            } else {
                NUMBERS_BASE
            };
            Some(chars[index])
        }
        0x10..=0x19 => {
            // QWERTY row
            let index = (scancode - 0x10) as usize;
            Some(process_alphabetic(scancode, QWERTY_BASE[index], modifiers))
        }
        0x1E..=0x26 => {
            // ASDF row
            let index = (scancode - 0x1E) as usize;
            Some(process_alphabetic(scancode, ASDF_BASE[index], modifiers))
        }
        0x2C..=0x32 => {
            // ZXCV row
            let index = (scancode - 0x2C) as usize;
            Some(process_alphabetic(scancode, ZXCV_BASE[index], modifiers))
        }
        0x0C | 0x0D | 0x1A | 0x1B | 0x27 | 0x28 | 0x29 | 0x2B | 0x33 | 0x34 | 0x35 => {
            for &(code, base, shifted) in PUNCTUATION.iter() {
                if code == scancode {
                    return Some(if shift_pressed { shifted } else { base });
                }
            }
            None
        }
        _ => {
            // Lookup in special keys
            SPECIAL_KEYS
                .iter()
                .find(|&&(code, _)| code == scancode)
                .map(|&(_, ascii)| ascii)
        }
    }
}

/// Handle keyboard interrupt and process scancodes
pub fn handle_keyboard_scancode(scancode: u8) {
    use petroleum::debug_log_no_alloc;
    debug_log_no_alloc!("Keyboard scancode received: {:x}", scancode);
    // Check if this is an extended scancode prefix
    let mut extended_flag = EXTENDED_SCANCODE.lock();
    if scancode == 0xE0 {
        *extended_flag = true;
        return;
    }

    let is_extended = *extended_flag;
    *extended_flag = false; // Reset for next
    drop(extended_flag); // Release lock

    let mut modifiers = MODIFIERS.lock();

    // Handle key press/release
    if is_extended {
        // Handle extended scancode
        match scancode {
            0x81..=0xFF => {
                let released_code = scancode & 0x7F;
                handle_extended_key_release(released_code, &mut modifiers);
            }
            _ => {
                handle_extended_key_press(scancode, &mut modifiers);
            }
        }
    } else {
        match scancode {
            // Key releases have high bit set (0x80 + scancode)
            0x81..=0xFF => {
                let released_code = scancode & 0x7F;
                handle_key_release(released_code, &mut modifiers);
            }

            // Key presses
            _ => {
                handle_key_press(scancode, &mut modifiers);
            }
        }
    }
}

fn handle_key_press(scancode: u8, modifiers: &mut KeyboardModifiers) {
    match scancode {
        // Modifier keys
        0x2A => modifiers.lshift = true, // Left Shift
        0x36 => modifiers.rshift = true, // Right Shift
        0x1D => modifiers.lctrl = true,  // Left Ctrl
        0x38 => modifiers.lalt = true,   // Left Alt

        // Lock keys (toggle on press)
        0x3A => modifiers.caps_lock = !modifiers.caps_lock,
        0x45 => modifiers.num_lock = !modifiers.num_lock,
        0x46 => modifiers.scroll_lock = !modifiers.scroll_lock,

        // Regular keys - convert to ASCII
        _ => {
            if let Some(ascii) = scancode_to_ascii(scancode, modifiers) {
                // Add to input buffer
                let mut buffer = INPUT_BUFFER.lock();
                if buffer.len() < 256 {
                    // Buffer size limit
                    buffer.push_back(ascii);
                }

                // Also add to string buffer (for line-based input)
                let mut str_buffer = INPUT_STRING_BUFFER.lock();
                if ascii == b'\n' || ascii == b'\r' {
                    // Line complete - you could signal here for read_line()
                } else if ascii == 0x08 {
                    // Backspace
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
        _ => {} // Other keys don't need release handling
    }
}

fn handle_extended_key_press(scancode: u8, modifiers: &mut KeyboardModifiers) {
    match scancode {
        0x1D => modifiers.rctrl = true, // Right Ctrl
        0x38 => modifiers.ralt = true,  // Right Alt
        // Add more extended keys as needed (arrows, etc.)
        _ => {} // Ignore unrecognized extended keys
    }
}

fn handle_extended_key_release(scancode: u8, modifiers: &mut KeyboardModifiers) {
    match scancode {
        0x1D => modifiers.rctrl = false, // Right Ctrl
        0x38 => modifiers.ralt = false,  // Right Alt
        _ => {}
    }
}

/// Read a character from keyboard input (blocking)
pub fn read_char() -> Option<u8> {
    let mut buffer = INPUT_BUFFER.lock();
    buffer.pop_front()
}

/// Drain the current line buffer (non-blocking)
/// This function copies the contents of the internal string buffer (accumulated
/// characters until newline) to the provided buffer and clears it.
/// Note: This does not block waiting for input - it drains whatever is available.
/// For blocking line reading, use keyboard::read_char() in a loop.
pub fn drain_line_buffer(buffer: &mut [u8]) -> usize {
    let mut str_buffer = INPUT_STRING_BUFFER.lock();
    // Copy available characters
    let str_bytes = str_buffer.as_bytes();
    let copy_len = str_bytes.len().min(buffer.len());
    buffer[..copy_len].copy_from_slice(&str_bytes[..copy_len]);
    let chars_copied = copy_len;

    // Clear the string buffer
    str_buffer.clear();

    chars_copied
}

/// Check if input is available (non-blocking)
pub fn input_available() -> bool {
    !INPUT_BUFFER.lock().is_empty()
}

/// Flush input buffer
pub fn flush_input() {
    INPUT_BUFFER.lock().clear();
    INPUT_STRING_BUFFER.lock().clear();
}

/// Get keyboard status
pub fn get_keyboard_status() -> KeyboardModifiers {
    *MODIFIERS.lock()
}

/// Initialize keyboard driver
pub fn init() {
    // Reset keyboard state
    flush_input();

    // Initialize keyboard instance lazily
    *KEYBOARD.lock() = Some(Keyboard::new(
        ScancodeSet1::default(),
        layouts::Us104Key {},
        pc_keyboard::HandleControl::Ignore,
    ));

    declare_init!("Keyboard input driver");
}

// Test functions
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scancode_conversion() {
        let mods = KeyboardModifiers::default();

        // Test basic letters
        assert_eq!(scancode_to_ascii(0x1E, &mods), Some(b'a'));
        assert_eq!(scancode_to_ascii(0x10, &mods), Some(b'q'));
        assert_eq!(scancode_to_ascii(0x39, &mods), Some(b' '));
        assert_eq!(scancode_to_ascii(0x1C, &mods), Some(b'\n'));

        // Invalid scancode
        assert_eq!(scancode_to_ascii(0xFF, &mods), None);
    }

    #[test]
    fn test_buffer_operations() {
        init();

        // Test write/read
        assert_eq!(read_char(), None);

        // Manually add to buffer for testing
        INPUT_BUFFER.lock().push_back(b't');
        assert!(input_available());
        assert_eq!(read_char(), Some(b't'));
        assert_eq!(read_char(), None);
    }
}
