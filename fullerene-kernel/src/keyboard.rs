//! Keyboard input driver for Fullerene OS
//!
//! This module provides keyboard input functionality including:
//! - PS/2 keyboard protocol handling
//! - Scan code to ASCII conversion
//! - Input buffer management
//! - Blocking/non-blocking input

#![no_std]

use alloc::collections::VecDeque;
use alloc::string::String;
use spin::Mutex;

/// Keyboard input buffer
static INPUT_BUFFER: Mutex<VecDeque<u8>> = Mutex::new(VecDeque::new());
static INPUT_STRING_BUFFER: Mutex<String> = Mutex::new(String::new());

/// Keyboard modifiers (shift, ctrl, alt, etc.)
#[derive(Debug, Clone, Copy, Default)]
struct KeyboardModifiers {
    lshift: bool,
    rshift: bool,
    lctrl: bool,
    rctrl: bool,
    lalt: bool,
    ralt: bool,
    caps_lock: bool,
    num_lock: bool,
    scroll_lock: bool,
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

/// Scancode set 1 to ASCII conversion
/// This is a simplified mapping - in a real system you'd handle extended codes
fn scancode_to_ascii(scancode: u8, modifiers: &KeyboardModifiers) -> Option<u8> {
    let shift_pressed = modifiers.lshift || modifiers.rshift;
    let caps_lock = modifiers.caps_lock;
    let ctrl_pressed = modifiers.lctrl || modifiers.rctrl;

    // Handle extended scancodes (0xE0 prefix)
    // This is simplified - real implementation needs state machine

    match scancode {
        // Numbers row
        0x02 => Some(if shift_pressed { b'!' } else { b'1' }),
        0x03 => Some(if shift_pressed { b'@' } else { b'2' }),
        0x04 => Some(if shift_pressed { b'#' } else { b'3' }),
        0x05 => Some(if shift_pressed { b'$' } else { b'4' }),
        0x06 => Some(if shift_pressed { b'%' } else { b'5' }),
        0x07 => Some(if shift_pressed { b'^' } else { b'6' }),
        0x08 => Some(if shift_pressed { b'&' } else { b'7' }),
        0x09 => Some(if shift_pressed { b'*' } else { b'8' }),
        0x0A => Some(if shift_pressed { b'(' } else { b'9' }),
        0x0B => Some(if shift_pressed { b')' } else { b'0' }),

        // QWERTY row
        0x10..=0x1C => {
            let base_chars = ['q', 'w', 'e', 'r', 't', 'y', 'u', 'i', 'o', 'p'];
            let mut ch = base_chars[(scancode - 0x10) as usize];
            if shift_pressed ^ caps_lock {
                ch = ch.to_ascii_uppercase();
            }
            if ctrl_pressed {
                return Some(ch as u8 & 0x1F); // Ctrl modifies ASCII
            }
            Some(ch as u8)
        }

        // ASDF row
        0x1E..=0x28 => {
            let base_chars = ['a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l'];
            let mut ch = base_chars[(scancode - 0x1E) as usize];
            if shift_pressed ^ caps_lock {
                ch = ch.to_ascii_uppercase();
            }
            if ctrl_pressed {
                return Some(ch as u8 & 0x1F);
            }
            Some(ch as u8)
        }

        // ZXCV row
        0x2C..=0x32 => {
            let base_chars = ['z', 'x', 'c', 'v', 'b', 'n', 'm'];
            let mut ch = base_chars[(scancode - 0x2C) as usize];
            if shift_pressed ^ caps_lock {
                ch = ch.to_ascii_uppercase();
            }
            if ctrl_pressed {
                return Some(ch as u8 & 0x1F);
            }
            Some(ch as u8)
        }

        // Space
        0x39 => Some(b' '),

        // Punctuation on shift
        0x0C => Some(if shift_pressed { b')' } else { b'-' }),
        0x0D => Some(if shift_pressed { b'=' } else { b'=' }),
        0x1A => Some(if shift_pressed { b'{' } else { b'[' }),
        0x1B => Some(if shift_pressed { b'}' } else { b']' }),
        0x27 => Some(if shift_pressed { b':' } else { b';' }),
        0x28 => Some(if shift_pressed { b'"' } else { b'\'' }),
        0x29 => Some(if shift_pressed { b'~' } else { b'`' }),
        0x2B => Some(if shift_pressed { b'|' } else { b'\\' }),
        0x33 => Some(if shift_pressed { b'<' } else { b',' }),
        0x34 => Some(if shift_pressed { b'>' } else { b'.' }),
        0x35 => Some(if shift_pressed { b'?' } else { b'/' }),

        // Special keys - we handle a few important ones
        0x1C => Some(b'\n'), // Enter
        0x0E => Some(0x08),  // Backspace

        _ => None,
    }
}

/// Handle keyboard interrupt and process scancodes
pub fn handle_keyboard_scancode(scancode: u8) {
    let mut modifiers = MODIFIERS.lock();

    // Handle key press/release
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

fn handle_key_press(scancode: u8, modifiers: &mut KeyboardModifiers) {
    match scancode {
        // Modifier keys
        0x2A => modifiers.lshift = true, // Left Shift
        0x36 => modifiers.rshift = true, // Right Shift
        0x1D => modifiers.lctrl = true,  // Left Ctrl
        0xE0 => modifiers.rctrl = true,  // Right Ctrl (extended)
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
        0xE0 => modifiers.rctrl = false, // Right Ctrl release (extended)
        0x38 => modifiers.lalt = false,
        _ => {} // Other keys don't need release handling
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
    let mut chars_copied = 0;

    // Copy available characters
    let str_bytes = str_buffer.as_bytes();
    let copy_len = str_bytes.len().min(buffer.len());
    buffer[..copy_len].copy_from_slice(&str_bytes[..copy_len]);
    chars_copied = copy_len;

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
    petroleum::serial::serial_log(format_args!("Keyboard input driver initialized\n"));
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
