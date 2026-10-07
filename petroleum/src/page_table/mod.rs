//! Page table management module.
//!
//! Shared pieces: the frame-allocator registry (`constants`), the page-backed
//! buffer (`page_buf`), and the two tiny facade functions. The x86 paging
//! tree lives in `x86_64/` and is globbed in for PC builds only.

pub mod constants;
pub mod page_buf;

#[cfg(not(target_arch = "aarch64"))]
pub mod x86_64;
#[cfg(not(target_arch = "aarch64"))]
pub use x86_64::*;

#[cfg(test)]
mod tests;

pub fn init_kernel_mapper() {}
pub fn find_free_virtual_address(_size: u64) -> Option<u64> {
    None
}
