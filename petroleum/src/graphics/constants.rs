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

// Text mode constants for BIOS video initialization

pub const VGA_SEQ_INDEX: u16 = 0x3C4;
pub const VGA_SEQ_DATA: u16 = 0x3C5;
pub const VGA_CRTC_INDEX: u16 = 0x3D4;
pub const VGA_CRTC_DATA: u16 = 0x3D5;
pub const VGA_GC_INDEX: u16 = 0x3CE;
pub const VGA_GC_DATA: u16 = 0x3CF;
pub const VGA_AC_INDEX: u16 = 0x3C0;
pub const VGA_AC_WRITE: u16 = 0x3C1;

// VGA configuration sequences for 80x25 text mode
pub const SEQUENCER_CONFIG: [(u8, u8); 5] = [
    (0x00, 0x03), (0x01, 0x00), (0x02, 0x03), (0x03, 0x00), (0x04, 0x02),
];
pub const CRTC_CONFIG: [(u8, u8); 18] = [
    (0x00, 0x5f), (0x01, 0x4f), (0x02, 0x50), (0x03, 0x82), (0x04, 0x55),
    (0x05, 0x81), (0x06, 0xbf), (0x07, 0x1f), (0x08, 0x00), (0x09, 0x4f),
    (0x10, 0x9c), (0x11, 0x8e), (0x12, 0x8f), (0x13, 0x28), (0x14, 0x1f),
    (0x15, 0x96), (0x16, 0xb9), (0x17, 0xa3),
];
pub const GRAPHICS_CONFIG: [(u8, u8); 9] = [
    (0x00, 0x00), (0x01, 0x00), (0x02, 0x00), (0x03, 0x00), (0x04, 0x00),
    (0x05, 0x10), (0x06, 0x0e), (0x07, 0x00), (0x08, 0xff),
];

pub const MISC_REGISTER_VALUE: u8 = 0x67;
pub const ATTRIBUTE_MODE_CONTROL_VALUE: u8 = 0x0c;

pub const BUFFER_HEIGHT: usize = 25;
pub const BUFFER_WIDTH: usize = 80;

pub const CURSOR_POS_LOW_REG: u8 = 0x0F;
pub const CURSOR_POS_HIGH_REG: u8 = 0x0E;
