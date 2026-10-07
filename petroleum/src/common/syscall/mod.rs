//! Low-level syscall instruction and compatibility wrappers.
//!
//! Branch by file: the x86 file carries the `syscall` instruction wrapper and
//! VDSO shortcuts; the ANE has no userspace trap wired yet, so its file is
//! empty. Syscall numbers are owned by the dependency-free `fullerene-abi`.

#[cfg(not(target_arch = "aarch64"))]
#[path = "x86_64.rs"]
mod imp;
#[cfg(target_arch = "aarch64")]
#[path = "aarch64.rs"]
mod imp;

pub use imp::*;
