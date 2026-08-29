//! Early UART transport.

const DATA: usize = 0x00;
const STATUS: usize = 0x1c;
const CONF0: usize = 0x20;
const CONF1: usize = 0x24;
const CLKDIV: usize = 0x2c;
const BASE: usize = 0x3ff4_0000;

pub fn init(base: usize) {
    let write =
        |offset: usize, value: u32| unsafe { ((base + offset) as *mut u32).write_volatile(value) };
    write(CONF1, 1 << 18 | 1 << 19);
    write(CONF0, 0);
    write(CLKDIV, 868);
}

pub fn putbyte(byte: u8) {
    // STATUS.TXFIFO_CNT is bits [23:16].  The ESP32 UART FIFO is 128
    // bytes deep; leave a little headroom so the write never overruns it.
    while (read(STATUS) >> 16) & 0xff >= 126 {
        core::hint::spin_loop();
    }
    unsafe { (BASE as *mut u32).add(DATA).write_volatile(u32::from(byte)) }
}

pub fn write_bytes(bytes: &[u8]) {
    for &byte in bytes {
        if byte == b'\n' {
            putbyte(b'\r');
        }
        putbyte(byte);
    }
}

pub fn write_str(value: &str) {
    write_bytes(value.as_bytes())
}

/// Write a small unsigned diagnostic value without pulling formatting code
/// into the early UART path.
pub fn write_u32(mut value: u32) {
    let mut digits = [0u8; 10];
    let mut cursor = digits.len();
    if value == 0 {
        putbyte(b'0');
        return;
    }
    while value != 0 {
        cursor -= 1;
        digits[cursor] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    write_bytes(&digits[cursor..]);
}

#[inline]
fn read(offset: usize) -> u32 {
    // Register constants above are byte offsets, not u32 indices.
    unsafe { ((BASE + offset) as *const u32).read_volatile() }
}

pub fn receive_byte() -> Option<u8> {
    // INT_RAW bit 0 means "RX FIFO full", not "a byte is available".
    // STATUS.RXFIFO_CNT (bits [7:0]) is the reliable polling primitive.
    (read(STATUS) & 0xff != 0).then(|| (read(DATA) & 0xff) as u8)
}
