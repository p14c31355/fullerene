//! Embedded runtime profiles.

#[cfg(target_arch = "aarch64")]
pub mod aarch64;
#[cfg(target_arch = "x86_64")]
pub mod x86_64;
#[cfg(target_arch = "xtensa")]
pub mod xtensa;

/// Monotonic timestamp source for the desktop runtime.
///
/// The x86 tree reads the CPU timestamp counter; the ANE reads a tick
/// counter its kernel-side timer driver advances. Same name on both sides.
#[cfg(target_arch = "aarch64")]
pub use aarch64::now_tsc;
#[cfg(target_arch = "x86_64")]
pub use x86_64::now_tsc;
