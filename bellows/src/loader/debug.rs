// bellows/src/loader/debug.rs

use x86_64::instructions::port::Port;

// Macro to write to port with wait
macro_rules! write_port {
    ($port:expr, $value:expr) => {
        unsafe {
            while (Port::<u8>::new(0x3FD).read() & 0x20) == 0 {}
            $port.write($value);
        }
    };
}

/// Writes a single byte to the COM1 serial port (0x3F8).
/// This is a very basic, early debug function that doesn't rely on any complex initialization.
pub fn debug_print_byte(byte: u8) {
    let mut port = Port::new(0x3F8);
    write_port!(port, byte);
}

/// Writes a string to the COM1 serial port.
pub fn debug_print_str(s: &str) {
    for byte in s.bytes() {
        debug_print_byte(byte);
    }
}

/// Prints a usize as hex (simple, no alloc).
pub fn debug_print_hex(value: usize) {
    debug_print_str("0x");
    let mut temp = value;
    let mut digits = [0u8; 16];
    let mut i = 0;
    if temp == 0 {
        debug_print_byte(b'0');
        return;
    }
    while temp > 0 && i < 16 {
        let digit = (temp % 16) as u8;
        digits[i] = if digit < 10 {
            b'0' + digit
        } else {
            b'a' + (digit - 10)
        };
        temp /= 16;
        i += 1;
    }
    for j in (0..i).rev() {
        debug_print_byte(digits[j]);
    }
}
