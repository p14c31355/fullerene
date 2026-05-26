//! PS/2 Keyboard Driver
//!
//! Scancode set 1 to ASCII conversion with input buffering, modifier tracking,
//! and key repeat support.

use alloc::collections::VecDeque;
use alloc::string::String;
use spin::Mutex;

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
    lshift: false, rshift: false, lctrl: false, rctrl: false,
    lalt: false, ralt: false, caps_lock: false, num_lock: false, scroll_lock: false,
});

/// Extended scancode flag
static EXTENDED_SCANCODE: Mutex<bool> = Mutex::new(false);

/// Key repeat state
static KEY_REPEAT: Mutex<KeyRepeatState> = Mutex::new(KeyRepeatState::new());
const KEY_REPEAT_DELAY_MS: u64 = 500;
const KEY_REPEAT_RATE_MS: u64 = 33;
static SYS_TICK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

#[derive(Debug, Clone, Copy)]
struct KeyRepeatState {
    last_scancode: u8,
    press_tick: u64,
    repeating: bool,
}

impl KeyRepeatState {
    const fn new() -> Self { Self { last_scancode: 0, press_tick: 0, repeating: false } }
}

// Scancode set 1 tables
const NUMBERS_BASE: &[u8] = b"1234567890";
const NUMBERS_SHIFT: &[u8] = b"!@#$%^&*()";
const QWERTY_BASE: &[u8] = b"qwertyuiop";
const ASDF_BASE: &[u8] = b"asdfghjkl";
const ZXCV_BASE: &[u8] = b"zxcvbnm";

const PUNCTUATION: &[(u8, u8, u8)] = &[
    (0x0C, b'-', b'_'), (0x0D, b'=', b'+'), (0x1A, b'[', b'{'), (0x1B, b']', b'}'),
    (0x27, b';', b':'), (0x28, b'\'', b'"'), (0x29, b'`', b'~'), (0x2B, b'\\', b'|'),
    (0x33, b',', b'<'), (0x34, b'.', b'>'), (0x35, b'/', b'?'),
];

const SPECIAL_KEYS: &[(u8, u8)] = &[
    (0x1C, b'\n'), (0x0E, 0x08), (0x0F, b'\t'), (0x01, 27), (0x39, b' '),
];

fn process_alphabetic(base: u8, modifiers: &KeyboardModifiers) -> u8 {
    let shift_pressed = modifiers.lshift || modifiers.rshift;
    let ctrl_pressed = modifiers.lctrl || modifiers.rctrl;
    let mut ch = base;
    if shift_pressed ^ modifiers.caps_lock { ch = ch.to_ascii_uppercase(); }
    if ctrl_pressed { ch &= 0x1F; }
    ch
}

fn scancode_to_ascii(scancode: u8, modifiers: &KeyboardModifiers) -> Option<u8> {
    let shift = modifiers.lshift || modifiers.rshift;
    match scancode {
        0x02..=0x0B => Some(if shift { NUMBERS_SHIFT[(scancode - 0x02) as usize] } else { NUMBERS_BASE[(scancode - 0x02) as usize] }),
        0x10..=0x19 => Some(process_alphabetic(QWERTY_BASE[(scancode - 0x10) as usize], modifiers)),
        0x1E..=0x26 => Some(process_alphabetic(ASDF_BASE[(scancode - 0x1E) as usize], modifiers)),
        0x2C..=0x32 => Some(process_alphabetic(ZXCV_BASE[(scancode - 0x2C) as usize], modifiers)),
        _ => PUNCTUATION.iter().find(|&&(c, _, _)| c == scancode).map(|&(_, b, s)| if shift { s } else { b })
            .or_else(|| SPECIAL_KEYS.iter().find(|&&(c, _)| c == scancode).map(|&(_, a)| a)),
    }
}

pub fn handle_keyboard_scancode(scancode: u8) {
    let mut ext = EXTENDED_SCANCODE.lock();
    if scancode == 0xE0 { *ext = true; return; }
    let is_ext = *ext; *ext = false; drop(ext);
    let mut mods = MODIFIERS.lock();

    if is_ext {
        if scancode & 0x80 != 0 { handle_ext_release(scancode & 0x7F, &mut mods); }
        else { handle_ext_press(scancode, &mut mods); }
    } else if scancode & 0x80 != 0 { handle_release(scancode & 0x7F, &mut mods); }
    else { handle_press(scancode, &mut mods); }
}

fn handle_press(scancode: u8, mods: &mut KeyboardModifiers) {
    match scancode {
        0x2A => mods.lshift = true,
        0x36 => mods.rshift = true,
        0x1D => mods.lctrl = true,
        0x38 => mods.lalt = true,
        0x3A => mods.caps_lock = !mods.caps_lock,
        0x45 => mods.num_lock = !mods.num_lock,
        0x46 => mods.scroll_lock = !mods.scroll_lock,
        _ => {
            track_repeat(scancode);
            if let Some(ascii) = scancode_to_ascii(scancode, mods) {
                let mut buf = INPUT_BUFFER.lock();
                if buf.len() < 256 { buf.push_back(ascii); }
                let mut sb = INPUT_STRING_BUFFER.lock();
                if ascii == 0x08 { sb.pop(); }
                else if sb.len() < 256 { sb.push(ascii as char); }
            }
        }
    }
}

fn handle_release(scancode: u8, mods: &mut KeyboardModifiers) {
    match scancode {
        0x2A => mods.lshift = false,
        0x36 => mods.rshift = false,
        0x1D => mods.lctrl = false,
        0x38 => mods.lalt = false,
        _ => {}
    }
    clear_repeat(scancode);
}

fn handle_ext_press(scancode: u8, mods: &mut KeyboardModifiers) {
    match scancode { 0x1D => mods.rctrl = true, 0x38 => mods.ralt = true, _ => {} }
}

fn handle_ext_release(scancode: u8, mods: &mut KeyboardModifiers) {
    match scancode { 0x1D => mods.rctrl = false, 0x38 => mods.ralt = false, _ => {} }
}

fn track_repeat(scancode: u8) {
    if matches!(scancode, 0x3A | 0x45 | 0x46) { return; }
    let mut r = KEY_REPEAT.lock();
    r.last_scancode = scancode;
    r.press_tick = SYS_TICK.load(core::sync::atomic::Ordering::Relaxed);
    r.repeating = false;
}

fn clear_repeat(scancode: u8) {
    let mut r = KEY_REPEAT.lock();
    if r.last_scancode == scancode { r.last_scancode = 0; r.repeating = false; }
}

pub fn read_char() -> Option<u8> { INPUT_BUFFER.lock().pop_front() }

pub fn input_available() -> bool { !INPUT_BUFFER.lock().is_empty() }

pub fn flush_input() { INPUT_BUFFER.lock().clear(); INPUT_STRING_BUFFER.lock().clear(); }

pub fn get_keyboard_status() -> KeyboardModifiers { *MODIFIERS.lock() }

pub fn drain_line_buffer(buffer: &mut [u8]) -> usize {
    let mut sb = INPUT_STRING_BUFFER.lock();
    let bytes = sb.as_bytes();
    let n = bytes.len().min(buffer.len());
    buffer[..n].copy_from_slice(&bytes[..n]);
    sb.clear();
    n
}

/// Update system tick for key repeat timing.
pub fn keyboard_tick(now: u64) {
    SYS_TICK.store(now, core::sync::atomic::Ordering::Relaxed);
}

/// Process key repeat — call from scheduler loop.
pub fn process_key_repeat() {
    let now = SYS_TICK.load(core::sync::atomic::Ordering::Relaxed);
    let mut r = KEY_REPEAT.lock();
    if r.last_scancode == 0 { return; }
    let elapsed = now.saturating_sub(r.press_tick);
    if !r.repeating && elapsed < KEY_REPEAT_DELAY_MS { return; }
    if r.repeating && elapsed < KEY_REPEAT_RATE_MS { return; }
    if !r.repeating { r.repeating = true; }
    r.press_tick = now;
    let sc = r.last_scancode;
    drop(r);
    let mods = MODIFIERS.lock();
    if let Some(ascii) = scancode_to_ascii(sc, &mods) {
        let mut buf = INPUT_BUFFER.lock();
        if buf.len() < 256 { buf.push_back(ascii); }
    }
}

pub fn init_keyboard() {
    flush_input();
    log::info!("PS/2 keyboard driver initialized");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_scancode_conversion() {
        let m = KeyboardModifiers::default();
        assert_eq!(scancode_to_ascii(0x1E, &m), Some(b'a'));
        assert_eq!(scancode_to_ascii(0x10, &m), Some(b'q'));
        assert_eq!(scancode_to_ascii(0x39, &m), Some(b' '));
        assert_eq!(scancode_to_ascii(0x1C, &m), Some(b'\n'));
    }
    #[test]
    fn test_buffer_operations() {
        init_keyboard();
        assert_eq!(read_char(), None);
        INPUT_BUFFER.lock().push_back(b't');
        assert!(input_available());
        assert_eq!(read_char(), Some(b't'));
    }
}