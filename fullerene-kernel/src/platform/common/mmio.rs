//! Volatile MMIO accessors for the platform drivers.
//!
//! dwmmc1, the Arasan SDHCI eMMC controller and the USB device core each
//! used to carry their own copy of these helpers; the bodies were
//! literally identical. This module is the single implementation - every
//! caller imports the names it uses (`rd`/`wr` wrappers that bind a fixed
//! base stay in the owning driver).

/// Volatile 8-bit read at `base + off`.
pub fn rd8(base: usize, off: usize) -> u8 {
    unsafe { core::ptr::read_volatile((base + off) as *const u8) }
}

/// Volatile 16-bit read at `base + off`.
pub fn rd16(base: usize, off: usize) -> u16 {
    unsafe { core::ptr::read_volatile((base + off) as *const u16) }
}

/// Volatile 32-bit read at `base + off`.
pub fn rd32(base: usize, off: usize) -> u32 {
    unsafe { core::ptr::read_volatile((base + off) as *const u32) }
}

/// Volatile 8-bit write at `base + off`.
pub fn wr8(base: usize, off: usize, v: u8) {
    unsafe { core::ptr::write_volatile((base + off) as *mut u8, v) }
}

/// Volatile 16-bit write at `base + off`.
pub fn wr16(base: usize, off: usize, v: u16) {
    unsafe { core::ptr::write_volatile((base + off) as *mut u16, v) }
}

/// Volatile 32-bit write at `base + off`.
pub fn wr32(base: usize, off: usize, v: u32) {
    unsafe { core::ptr::write_volatile((base + off) as *mut u32, v) }
}
