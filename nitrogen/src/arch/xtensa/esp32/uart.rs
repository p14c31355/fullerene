//! Early UART transport.

const DATA: usize = 0x00;
const INT_RAW: usize = 0x08;
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
    while read(STATUS) & 0x3ff0_0000 != 0 {}
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

#[inline]
fn read(offset: usize) -> u32 {
    unsafe { (BASE as *const u32).add(offset).read_volatile() }
}

pub fn receive_byte() -> Option<u8> {
    (read(INT_RAW) & 1 != 0).then(|| (read(DATA) & 0xff) as u8)
}
