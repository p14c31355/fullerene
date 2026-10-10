//! PS/2 keyboard shim (no PS/2 controller on the ANE).

/// No foreground terminal gate on the ANE; nothing is ever queued.
pub fn read_char() -> Option<u8> {
    None
}

/// No queued input.
pub fn input_available() -> bool {
    false
}

/// No decoded bytes to pop.
pub fn pop_input_char_unchecked() -> Option<u8> {
    None
}

/// No queue to rewrite.
pub fn replace_input_byte(_target: u8, _replacement: &[u8]) -> bool {
    false
}
