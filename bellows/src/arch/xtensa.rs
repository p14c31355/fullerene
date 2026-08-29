//! Bellows' Xtensa platform bring-up boundary.

#[cfg(all(target_arch = "xtensa", feature = "esp32"))]
pub mod esp32;
