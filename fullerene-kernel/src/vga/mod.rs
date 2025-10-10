pub use petroleum::{Color, ColorCode, ScreenChar, TextBufferOperations};

const BUFFER_HEIGHT: usize = 25;
const BUFFER_WIDTH: usize = 80;

pub mod buffer;
pub mod ops;

pub use buffer::*;
pub use ops::*;

#[cfg(test)]
mod tests;
