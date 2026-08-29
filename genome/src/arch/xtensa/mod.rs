//! Xtensa storage adapters.

#[cfg(all(target_arch = "xtensa", feature = "esp32"))]
pub mod esp32;
