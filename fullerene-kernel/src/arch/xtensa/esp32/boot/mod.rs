//! Boot metadata shared with image tooling.

pub const ENTRY_SYMBOL: &str = "_start";
pub const ROM_MAGIC: [u8; 8] = [0xe9, 0xff, 0xff, 0xff, 0x00, 0x80, 0x40, 0x00];
