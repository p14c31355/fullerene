//! Xtensa shell adaptors.

#[cfg(all(target_arch = "xtensa", feature = "esp32"))]
pub mod esp32;
