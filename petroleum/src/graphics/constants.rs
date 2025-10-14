/// VGA mode 13h constants for graphics mode setup
/// This module consolidates VGA mode 13h configuration constants
/// for better code organization and reusability in the petroleum crate.

// VGA framebuffer address for mode 13h (320x200, 256 colors)
pub const VGA_MODE13H_ADDRESS: u64 = 0xA0000;

// VGA mode 13h width in pixels
pub const VGA_MODE13H_WIDTH: u32 = 320;

// VGA mode 13h height in pixels
pub const VGA_MODE13H_HEIGHT: u32 = 200;

// VGA mode 13h bits per pixel (8 BPP for 256-color palette)
pub const VGA_MODE13H_BPP: u32 = 8;

// VGA mode 13h stride in bytes (320 bytes per scan line)
pub const VGA_MODE13H_STRIDE: u32 = 320;
