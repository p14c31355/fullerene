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

pub mod controller;
pub mod corb;
pub mod codec;
pub mod route;
pub mod dma;
pub mod diagnostics;

pub use controller::HdaController;
pub use dma::DmaRegion;

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