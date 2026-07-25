//! Format-independent documents produced by decoders.

use alloc::string::String;
use alloc::vec::Vec;

pub enum Document {
    Text(TextDocument),
    Launch(LaunchTarget),
    Binary(BinaryDocument),
}

pub enum LaunchTarget {
    Wasm { path: String, args: Vec<String> },
}

pub struct TextDocument {
    pub text: String,
}

pub struct BinaryDocument {
    pub size: u64,
    pub preview: Vec<u8>,
}
