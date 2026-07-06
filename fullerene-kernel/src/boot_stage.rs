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
    let phys = unsafe { petroleum::page_table::kernel::init::BOOT_FB_PHYS };
    let stride_px = unsafe { petroleum::page_table::kernel::init::BOOT_FB_STRIDE_PX } as usize;
    // Diagnostic: write a single bright-green pixel at (400, 0) using phys
    // to confirm fb_stage_pixel is reached and BOOT_FB_PHYS is readable.
    // Position 400 is right after the 200-red + 200-blue init_and_jump blocks.
    if phys >= 0x100_000 && phys <= 0x10_0000_0000 {
        let p = unsafe { &mut *(phys as *mut u32) };
        *p = 0x0000FF00u32; // green
        unsafe { core::arch::x86_64::_mm_sfence() };
    }
    if phys < 0x100_000 || phys > 0x10_0000_0000 || stride_px < 320 || stride_px > 16384 {
        return;
    }
    let fb_va = phys;
    if fb_va == 0 || fb_va >= 0x10000_0000_0000 {
        return;
    }
    let fb_ptr = fb_va as *mut u32;
    let color = stage_color(stage);
    let x0 = ((stage as usize).saturating_mul(35)).min(stride_px.saturating_sub(30));
    for dy in 0..16usize {
        let row = dy.saturating_mul(stride_px);
        for dx in 0..30usize {
            unsafe { core::ptr::write_volatile(fb_ptr.add(row + x0 + dx), color) };
        }
    }
    unsafe { core::arch::x86_64::_mm_sfence() };
}

fn stage_color(stage: u8) -> u32 {
    match stage {
        0  => 0x00_00_00_FF,
        1  => 0x00_00_00_44,
        2  => 0x00_00_00_88,
        3  => 0x00_88_88_00,
        4  => 0x00_00_44_00,
        5  => 0x00_00_88_00,
        6  => 0x00_00_88_44,
        7  => 0x00_00_88_88,
        8  => 0x00_00_44_88,
        9  => 0x00_00_00_88,
        10 => 0x00_00_00_AA,
        11 => 0x00_00_00_55,
        12 => 0x00_88_00_88,
        13 => 0x00_44_00_88,
        14 => 0x00_44_00_44,
        15 => 0x00_55_55_55,
        _  => 0x00_FF_00_FF,
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
