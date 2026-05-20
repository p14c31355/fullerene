//! # Early Boot Framebuffer Detection
//!
//! Boot-phase framebuffer **detection and configuration only**.
//!
//! This module handles:
//!
//! - UEFI GOP (Graphics Output Protocol) detection
//! - VGA mode 13h setup
//! - Fallback framebuffer detection (QEMU std-vga, Cirrus, etc.)
//!
//! ## What does NOT belong here
//!
//! - `FramebufferWriter` (renderer) — belongs in `graphics::framebuffer`
//! - `Renderer` trait implementation — runtime layer
//! - Console / text rendering on framebuffer — runtime layer
//!
//! ## Contract
//!
//! After the world switch, the kernel should NOT call these functions.
//! The bootloader passes a `FullereneFramebufferConfig` to the kernel,
//! and the kernel creates its own renderer from that config.

use crate::common::uefi::FullereneFramebufferConfig;
use crate::graphics::color::{ColorScheme, FramebufferInfo};
use crate::common::EfiGraphicsPixelFormat;

/// Result of early framebuffer detection.
pub struct EarlyFramebufferInfo {
    /// The framebuffer configuration (address, dimensions, format).
    pub config: FullereneFramebufferConfig,
    /// Whether this framebuffer was obtained via UEFI GOP.
    pub from_uefi: bool,
}

/// Attempt to detect a usable framebuffer using UEFI GOP.
///
/// Returns `Some(config)` if a GOP framebuffer is available.
///
/// # Safety
///
/// `system_table` must point to a valid UEFI system table with active BootServices.
pub unsafe fn detect_uefi_gop(
    system_table: *mut crate::common::EfiSystemTable,
) -> Option<FullereneFramebufferConfig> {
    if system_table.is_null() {
        return None;
    }
    let st = unsafe { &*system_table };
    let result = crate::graphics::uefi::init_gop_framebuffer(st);
    match result {
        Some(config) => {
            crate::serial::_print(format_args!(
                "[early::fb] GOP detected: {:#x} ({}x{}, stride={})\n",
                config.address, config.width, config.height, config.stride
            ));
            Some(config)
        }
        None => {
            crate::serial::_print(format_args!(
                "[early::fb] GOP detection failed\n"
            ));
            None
        }
    }
}

/// Detect and initialise VGA mode 13h (320x200, 256 colours).
///
/// This is useful on legacy BIOS or when UEFI GOP is unavailable.
/// The detected configuration can be used to create a `FramebufferWriter<u8>`
/// in the runtime layer.
pub fn detect_vga_mode_13h() -> Option<EarlyFramebufferInfo> {
    // Mode 13h: 320x200, 8-bit indexed colour at 0xA0000
    crate::graphics::setup::setup_vga_mode_13h();
    let config = FullereneFramebufferConfig {
        address: 0xA0000,
        width: 320,
        height: 200,
        pixel_format: EfiGraphicsPixelFormat::PixelBitMask,
        bpp: 8,
        stride: 320,
    };
    crate::serial::_print(format_args!(
        "[early::fb] VGA mode 13h configured: {}x{}\n",
        config.width, config.height
    ));
    Some(EarlyFramebufferInfo {
        config,
        from_uefi: false,
    })
}

/// Try to detect a QEMU std-vga / Bochs VBE framebuffer.
///
/// Uses the safe `test_qemu_framebuffer_access()` which validates the address
/// without performing a direct WC-memory probe (reads on WC memory always
/// return 0, so probing is meaningless and potentially dangerous).
///
/// Returns a `FramebufferInfo` suitable for a runtime `FramebufferWriter<u32>`.
pub fn detect_qemu_std_vga() -> Option<FramebufferInfo> {
    for qcfg in crate::QEMU_CONFIGS.iter() {
        if !crate::graphics::uefi::test_qemu_framebuffer_access(qcfg.address) {
            continue;
        }
        crate::serial::_print(format_args!(
            "[early::fb] QEMU std-vga detected at {:#x}: {}x{}x{}\n",
            qcfg.address, qcfg.width, qcfg.height, qcfg.bpp
        ));
        return Some(FramebufferInfo {
            address: qcfg.address,
            width: qcfg.width,
            height: qcfg.height,
            stride: qcfg.width,
            pixel_format: Some(EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor),
            colors: ColorScheme::UEFI_GREEN_ON_BLACK,
        });
    }
    None
}

// ── Re-exports from graphics::setup (VGA mode functions) ─────────────
// These are boot-phase only. The kernel must not call them after the
// world switch.

pub use crate::graphics::setup::{
    setup_vga_mode_13h,
    setup_cirrus_vga_mode,
    detect_and_init_vga_graphics,
    detect_cirrus_vga,
    init_vga_graphics,
    init_vga_text_mode,
};

// ── Re-exports from crate::boot ──────────────────────────────────────
// Boot-phase framebuffer console creation. After the world switch,
// the kernel should use its own `graphics::PRIMARY_RENDERER`.

pub use crate::boot::{
    create_primary_console,
    initialize_vga_fallback,
};

/// Initialise the framebuffer using whichever method is available.
///
/// Priority:
/// 1. UEFI GOP (from system table)
/// 2. QEMU std-vga fallback (PCI probing)
/// 3. Legacy VGA mode 13h
///
/// Returns the detected framebuffer info, or `None` if no method succeeds.
///
/// # Safety
///
/// `system_table` must be valid if provided (non-null).
pub unsafe fn init_early_framebuffer(
    system_table: Option<*mut crate::common::EfiSystemTable>,
) -> Option<EarlyFramebufferInfo> {
    // 1. UEFI GOP
    if let Some(st) = system_table {
        if let Some(config) = detect_uefi_gop(st) {
            return Some(EarlyFramebufferInfo {
                config,
                from_uefi: true,
            });
        }
    }

    // 2. QEMU std-vga
    if let Some(qemu_info) = detect_qemu_std_vga() {
        let config = FullereneFramebufferConfig {
            address: qemu_info.address,
            width: qemu_info.width,
            height: qemu_info.height,
            pixel_format: qemu_info.pixel_format
                .unwrap_or(EfiGraphicsPixelFormat::PixelBlueGreenRedReserved8BitPerColor),
            bpp: 32,
            stride: qemu_info.stride,
        };
        return Some(EarlyFramebufferInfo {
            config,
            from_uefi: false,
        });
    }

    // 3. VGA mode 13h
    detect_vga_mode_13h()
}