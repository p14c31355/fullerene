//! Boot stage tracking for post-mortem debugging.
//!
//! Tracks the furthest boot stage reached.  The current stage can be
//! queried at any time and is included as a `LastStage=…` line in
//! `/bootlog.txt` on panic or clean shutdown.
//!
//! # Usage
//!
//! ```ignore
//! set_boot_stage(BootStage::HeapReady);
//! ```
//!
//! The macro `boot_stage!` also writes a human-readable line to `klog`.

use core::sync::atomic::{AtomicU8, Ordering};

// ── Framebuffer diagnostic pixel (written on each stage advance) ──
//
// Writes a single coloured pixel at column `stage × 8` on the top row
// of the GOP framebuffer via the identity-mapped higher-half VA.
// This is NOT a heartbeat — it's an immutable trace that persists once
// written.  If the system hangs in a busy-wait, the last pixel tells
// you the last stage that completed.
//
// No allocation, no locks, no heap access.  Safe to call from any
// context including the panic handler.
fn fb_stage_pixel(stage: u8) {
    let phys = unsafe { crate::graphics::discovery::STORED_FB_PHYS };
    let w    = unsafe { crate::graphics::discovery::STORED_FB_WIDTH };
    let stride_raw = unsafe { crate::graphics::discovery::STORED_FB_STRIDE };

    // FB not yet discovered (stage < GraphicsReady) — skip silently.
    if phys < 0x100_000 || phys > 0x10_0000_0000 || w < 80 || stride_raw < 320 {
        return;
    }
    let stride = usize::try_from(stride_raw).unwrap_or(w as usize * 4);

    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
    let fb_va = phys + off;
    if fb_va == 0 || fb_va >= 0x10000_0000_0000 {
        return;
    }

    let col = (stage as usize).saturating_mul(8);
    // Draw a 2×2 block so it's visible even at low resolution.
    for dy in 0..2usize {
        let idx = dy * (stride / 4) + col;
        let px = fb_va as *mut u32;
        unsafe { core::ptr::write_volatile(px.add(idx), stage_color(stage)) };
        let px2 = fb_va as *mut u32;
        unsafe { core::ptr::write_volatile(px2.add(idx + 1), stage_color(stage)) };
    }
}

/// Boot-stage colour (must match `main.rs` panic_screen colours).
fn stage_color(stage: u8) -> u32 {
    match stage {
        0   => 0x00_00_00_FF,  // bright red
        1   => 0x00_00_00_44,  // dark blue
        2   => 0x00_00_00_88,  // blue
        3   => 0x00_88_88_00,  // cyan
        4   => 0x00_00_44_00,  // dark green
        5   => 0x00_00_88_00,  // green
        6   => 0x00_00_88_44,  // yellow-green
        7   => 0x00_00_88_88,  // yellow
        8   => 0x00_00_44_88,  // orange
        9   => 0x00_00_00_88,  // dark orange
        10  => 0x00_00_00_AA,  // red
        11  => 0x00_00_00_55,  // dark red
        12  => 0x00_88_00_88,  // magenta
        13  => 0x00_44_00_88,  // pink
        14  => 0x00_44_00_44,  // purple
        15  => 0x00_55_55_55,  // gray
        _   => 0x00_FF_00_FF,  // bright magenta
    }
}

// ── Boot stage enumeration ─────────────────────────────────────────

/// Monotonically increasing boot stages.
///
/// The numeric value encodes the order — higher values represent later
/// stages.  `0` means not yet set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum BootStage {
    KernelEntry = 1,
    MemoryMapped = 2,
    HeapReady = 3,
    InterruptsReady = 4,
    KernelContextReady = 5,
    PciBarsReady = 6,
    GraphicsReady = 7,
    InputReady = 8,
    ProcessReady = 9,
    SyscallReady = 10,
    FilesystemReady = 11,
    LoaderReady = 12,
    GuiReady = 13,
    TaskManagerReady = 14,
    AppRunnerReady = 15,
    ShellRunning = 16,
    Panic = 255,
}

impl BootStage {
    /// Human-readable label for use in log output.
    pub fn label(self) -> &'static str {
        match self {
            BootStage::KernelEntry => "KernelEntry",
            BootStage::MemoryMapped => "MemoryMapped",
            BootStage::HeapReady => "HeapReady",
            BootStage::InterruptsReady => "InterruptsReady",
            BootStage::KernelContextReady => "KernelContextReady",
            BootStage::PciBarsReady => "PciBarsReady",
            BootStage::GraphicsReady => "GraphicsReady",
            BootStage::InputReady => "InputReady",
            BootStage::ProcessReady => "ProcessReady",
            BootStage::SyscallReady => "SyscallReady",
            BootStage::FilesystemReady => "FilesystemReady",
            BootStage::LoaderReady => "LoaderReady",
            BootStage::GuiReady => "GuiReady",
            BootStage::TaskManagerReady => "TaskManagerReady",
            BootStage::AppRunnerReady => "AppRunnerReady",
            BootStage::ShellRunning => "ShellRunning",
            BootStage::Panic => "Panic",
        }
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(BootStage::KernelEntry),
            2 => Some(BootStage::MemoryMapped),
            3 => Some(BootStage::HeapReady),
            4 => Some(BootStage::InterruptsReady),
            5 => Some(BootStage::KernelContextReady),
            6 => Some(BootStage::PciBarsReady),
            7 => Some(BootStage::GraphicsReady),
            8 => Some(BootStage::InputReady),
            9 => Some(BootStage::ProcessReady),
            10 => Some(BootStage::SyscallReady),
            11 => Some(BootStage::FilesystemReady),
            12 => Some(BootStage::LoaderReady),
            13 => Some(BootStage::GuiReady),
            14 => Some(BootStage::TaskManagerReady),
            15 => Some(BootStage::AppRunnerReady),
            16 => Some(BootStage::ShellRunning),
            255 => Some(BootStage::Panic),
            _ => None,
        }
    }
}

// ── Global state ────────────────────────────────────────────────────

/// Last boot stage reached (atomic, lock-free for panic-safety).
static LAST_STAGE: AtomicU8 = AtomicU8::new(0);

/// Update the global boot stage and emit a `klog` line.
pub fn set_boot_stage(stage: BootStage) {
    let prev = LAST_STAGE.fetch_max(stage as u8, Ordering::Release);
    // Only log if we actually advanced (monotonic).
    if stage as u8 > prev {
        crate::klog::write_fmt(format_args!("[BOOT] {}\n", stage.label()));
        // ── Diagnostic pixel on framebuffer (if available) ──
        fb_stage_pixel(stage as u8);
    }
}

/// Get the last boot stage reached.
pub fn last_stage() -> Option<BootStage> {
    let raw = LAST_STAGE.load(Ordering::Acquire);
    BootStage::from_u8(raw)
}

/// Format a `LastStage=…` line for inclusion in `/bootlog.txt`.
pub fn last_stage_line() -> alloc::string::String {
    match last_stage() {
        Some(s) => alloc::format!("LastStage={}\n", s.label()),
        None => alloc::string::String::from("LastStage=Unknown\n"),
    }
}

// ── Convenience macro ──────────────────────────────────────────────

/// Set the boot stage and also emit a human-readable `klog` line.
///
/// ```ignore
/// boot_stage!(BootStage::HeapReady);
/// ```
#[macro_export]
macro_rules! boot_stage {
    ($stage:expr) => {
        $crate::boot_stage::set_boot_stage($stage);
    };
}
