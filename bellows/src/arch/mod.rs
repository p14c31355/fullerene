//! Architecture-specific bootloader policy.
//!
//! Bellows remains a single crate, but its entry policy is kept behind this
//! directory so adding an AArch64 loader does not require another package.

pub mod x86_64;
