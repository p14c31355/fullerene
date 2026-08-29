//! Architecture-specific kernel backends.

#[cfg(all(target_arch = "xtensa", feature = "esp32"))]
pub mod xtensa;
