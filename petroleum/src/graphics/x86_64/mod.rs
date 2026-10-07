//! x86 VGA/UEFI graphics backends: only compiled on x86 targets.
//!
//! Globbed into `graphics`, so historic paths (`graphics::setup::...`,
//! `crate::graphics::framebuffer::...`) stay valid on PC builds.

pub mod boot_screen;
pub mod framebuffer;
pub mod framebuffer_mapper;
pub mod registers;
pub mod setup;
pub mod uefi;

mod vga_initializer;

pub use framebuffer::UefiFramebufferWriter;
pub use framebuffer::*;
pub use framebuffer_mapper::{CacheMode, FramebufferMapper};
pub use setup::{
    detect_and_init_vga_graphics, detect_cirrus_vga, init_vga_graphics, init_vga_text_mode,
    setup_cirrus_vga_mode,
};
