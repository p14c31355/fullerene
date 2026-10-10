//! VGA text-mode debug output for post-ExitBootServices diagnosis.
//! Writes directly to the VGA text buffer at 0xB8000.
//! Does NOT depend on any UEFI services, heap, or alloc.

use sealant::{FramebufferRegion, Permissions};

const VGA_ADDRESS: usize = 0xB8000;

fn vga_region() -> FramebufferRegion<'static> {
    unsafe {
        FramebufferRegion::from_address(VGA_ADDRESS, 80 * 25 * 2, Permissions::READ_WRITE)
            .expect("invalid VGA framebuffer region")
    }
}

/// Write a single ASCII character to the VGA text buffer at position (row, col).
/// Row 0-24, col 0-79.  White-on-black attribute.
#[inline]
pub fn vga_putc(row: usize, col: usize, c: u8) {
    if row >= 25 || col >= 80 {
        return;
    }
    let _ = vga_region().write_volatile_at(
        (row * 80 + col) * core::mem::size_of::<u16>(),
        (c as u16) | 0x0F00,
    );
}

/// Write a byte string to the VGA text buffer starting at position (row, col).
/// Returns the next column after the last written character.
pub fn vga_puts(row: usize, col: usize, s: &[u8]) -> usize {
    let mut c = col;
    for &ch in s {
        if ch == b'\n' {
            break;
        }
        if c >= 80 {
            break;
        }
        vga_putc(row, c, ch);
        c += 1;
    }
    c
}

/// Write a u64 as hex at (row, col).  Returns next column.
pub fn vga_puthex(row: usize, col: usize, val: u64) -> usize {
    let hex = b"0123456789ABCDEF";
    let mut c = col;
    for i in (0..16).rev() {
        let nibble = ((val >> (i * 4)) & 0xF) as usize;
        vga_putc(row, c, hex[nibble]);
        c += 1;
    }
    c
}

/// Macro to print a message to VGA at a fixed row.
/// Usage: vga_msg!(0, b"MSG");
#[macro_export]
macro_rules! vga_msg {
    ($row:expr, $msg:expr) => {
        $crate::vga_debug::vga_puts($row, 0, $msg);
    };
}
