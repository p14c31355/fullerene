//! Architecture-facing Nitrogen transport mechanisms.
//!
//! The split is directory-based: `x86_64` keeps the full PC driver set, the
//! ANE has a small input/telemetry subset under `aarch64`, and the ESP32
//! drivers live under `xtensa`.

#[cfg(target_arch = "aarch64")]
pub mod aarch64;
#[cfg(target_arch = "x86_64")]
pub mod x86_64;
#[cfg(target_arch = "xtensa")]
pub mod xtensa;
