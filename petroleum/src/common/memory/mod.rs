//! Address translation and user-memory validation.
//!
//! Branch by file: the x86 file carries the page-table walkers and user-range
//! validation; the ANE file carries the flat identity translation core the
//! phone actually uses. Both provide the same names, so callers never care
//! which one is live.

#[cfg(not(target_arch = "aarch64"))]
#[path = "x86_64.rs"]
mod imp;
#[cfg(target_arch = "aarch64")]
#[path = "aarch64.rs"]
mod imp;

pub use imp::*;
