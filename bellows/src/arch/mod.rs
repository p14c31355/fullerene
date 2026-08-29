//! Architecture-specific bootloader policy.
//!
//! Bellows remains a single crate, but its entry policy is kept behind this
//! directory so adding an AArch64 loader does not require another package.

#[cfg(target_arch = "x86_64")]
pub mod x86_64;

#[cfg(all(target_arch = "xtensa", feature = "esp32"))]
pub mod xtensa;
