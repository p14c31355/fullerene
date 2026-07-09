//! # Nitrogen HDA (High Definition Audio) Subsystem
//!
//! Pure hardware-mechanism layer for Intel HDA controllers and codecs.
//! This module provides:
//!
//! - `HdaController` — a context struct that owns all HDA state
//!   (replaces the previous pattern of scattered global statics).
//! - CORB/RIRB verb engine
//! - Codec graph enumeration (widget tree discovery)
//! - DAC→Pin route finding
//! - DMA buffer (BDL) management
//! - Diagnostic inventory dump
//!
//! ## Design
//!
//! ```text
//! HdaController
//!  ├── corb::CorbEngine      (verb send/receive)
//!  ├── codec::CodecGraph      (widget enumeration)
//!  ├── route::RouteFinder     (DAC→Pin path selection)
//!  ├── dma::DmaEngine         (BDL programming, buffer feed/poll)
//!  └── diagnostics            (dump_codec_inventory, etc.)
//! ```
//!
//! The caller (kernel) is responsible for:
//! - Providing the physical-memory offset (for MMIO virtual address
//!   computation).
//! - Allocating contiguous physical pages for CORB, RIRB, and DMA
//!   buffers, and passing them in as `DmaRegion` values.

pub mod codec;
pub mod controller;
pub mod corb;
pub mod diagnostics;
pub mod dma;
pub mod route;

pub use controller::HdaController;
pub use dma::DmaRegion;

// ── Shared MMIO helpers (used by controller, corb, dma) ──────────

#[macro_export]
macro_rules! make_mmio_helpers {
    () => {
        #[allow(dead_code)]
        #[inline]
        unsafe fn mmio_read32(mmio: *mut u8, offset: usize) -> u32 {
            let val = unsafe { core::ptr::read_volatile(mmio.add(offset) as *const u32) };
            if val == 0xFFFF_FFFF {
                crate::debug::print("hda", "MMIO read returned 0xFFFF_FFFF (master abort?)");
            }
            val
        }
        #[allow(dead_code)]
        #[inline]
        unsafe fn mmio_read16(mmio: *mut u8, offset: usize) -> u16 {
            let val = unsafe { core::ptr::read_volatile(mmio.add(offset) as *const u16) };
            if val as u32 == 0xFFFF {
                crate::debug::print("hda", "MMIO read16 returned 0xFFFF (master abort?)");
            }
            val
        }
        #[allow(dead_code)]
        #[inline]
        unsafe fn mmio_read8(mmio: *mut u8, offset: usize) -> u8 {
            let val = unsafe { core::ptr::read_volatile(mmio.add(offset)) };
            if val as u32 == 0xFF {
                crate::debug::print("hda", "MMIO read8 returned 0xFF (master abort?)");
            }
            val
        }
        #[allow(dead_code)]
        #[inline]
        unsafe fn mmio_write32(mmio: *mut u8, offset: usize, val: u32) {
            unsafe { core::ptr::write_volatile(mmio.add(offset) as *mut u32, val) }
        }
        #[allow(dead_code)]
        #[inline]
        unsafe fn mmio_write16(mmio: *mut u8, offset: usize, val: u16) {
            unsafe { core::ptr::write_volatile(mmio.add(offset) as *mut u16, val) }
        }
        #[allow(dead_code)]
        #[inline]
        unsafe fn mmio_write8(mmio: *mut u8, offset: usize, val: u8) {
            unsafe { core::ptr::write_volatile(mmio.add(offset), val) }
        }
    };
}

/// Widget types as defined by the HDA specification §7.3.4.
pub mod widget_type {
    pub const AUDIO_OUTPUT: u32 = 0x0;
    pub const AUDIO_INPUT: u32 = 0x1;
    pub const AUDIO_MIXER: u32 = 0x2;
    pub const AUDIO_SELECTOR: u32 = 0x3;
    pub const PIN_COMPLEX: u32 = 0x4;
    pub const POWER_WIDGET: u32 = 0x5;
    pub const VOLUME_KNOB: u32 = 0x6;
    pub const BEEP_GENERATOR: u32 = 0x7;
    pub const VENDOR_DEFINED: u32 = 0xF;
    pub const AFG: u32 = 0x1;
}
