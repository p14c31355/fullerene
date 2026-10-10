//! Architecture-specific platform layer.
//!
//! This is the single place where the architecture is decided: each side lives
//! in its own directory (`x86_64/`, `aarch64/`) and lib.rs re-exports the
//! chosen tree at the crate root. Shared code therefore keeps naming
//! `crate::serial::serial_log` or `crate::halt_loop` without carrying any cfg
//! of its own.

#[cfg(target_arch = "aarch64")]
pub mod aarch64;
#[cfg(not(target_arch = "aarch64"))]
pub mod x86_64;

#[cfg(target_arch = "aarch64")]
pub use aarch64::set_hardware_cursor;
#[cfg(not(target_arch = "aarch64"))]
pub use x86_64::set_hardware_cursor;
