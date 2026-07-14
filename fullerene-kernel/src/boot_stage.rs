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

/// Redraw the allocation-free splash through the bootstrap's direct mapping.
fn draw_boot_stage(stage: BootStage) {
    let Some(framebuffer) = crate::graphics::discovery::direct_boot_framebuffer() else {
        return;
    };
    let completed = if stage == BootStage::Panic {
        petroleum::graphics::boot_screen::KERNEL_STAGE_COUNT
    } else {
        (stage as u8).min(petroleum::graphics::boot_screen::KERNEL_STAGE_COUNT)
    };
    unsafe {
        framebuffer.draw_stage(
            completed,
            petroleum::graphics::boot_screen::KERNEL_STAGE_COUNT,
            stage.screen_label(),
        );
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

    /// Short uppercase label that fits on the boot splash at low resolutions.
    pub fn screen_label(self) -> &'static [u8] {
        match self {
            BootStage::KernelEntry => b"KERNEL ENTRY",
            BootStage::MemoryMapped => b"MEMORY MAPPED",
            BootStage::HeapReady => b"HEAP READY",
            BootStage::InterruptsReady => b"INTERRUPTS READY",
            BootStage::KernelContextReady => b"KERNEL CONTEXT",
            BootStage::PciBarsReady => b"PCI DEVICES",
            BootStage::GraphicsReady => b"GRAPHICS READY",
            BootStage::InputReady => b"INPUT DEVICES",
            BootStage::ProcessReady => b"PROCESS MANAGER",
            BootStage::SyscallReady => b"SYSTEM CALLS",
            BootStage::FilesystemReady => b"FILESYSTEM",
            BootStage::LoaderReady => b"PROGRAM LOADER",
            BootStage::GuiReady => b"DESKTOP SERVICES",
            BootStage::TaskManagerReady => b"TASK MANAGER",
            BootStage::AppRunnerReady => b"STARTING DESKTOP",
            BootStage::ShellRunning => b"SHELL RUNNING",
            BootStage::Panic => b"KERNEL PANIC",
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
        draw_boot_stage(stage);
    }
}

/// Update just the label text on the boot screen without advancing the stage
/// counter.  Use this for intermediate init steps (initramfs, USB, SD, WiFi)
/// that perform real work but do not map to a BootStage value.
///
/// The progress bar stays at the last-committed stage position.
pub fn draw_boot_label(label: &[u8]) {
    let Some(framebuffer) = crate::graphics::discovery::direct_boot_framebuffer() else {
        return;
    };
    let prev = LAST_STAGE.load(Ordering::Acquire);
    let completed = prev.min(petroleum::graphics::boot_screen::KERNEL_STAGE_COUNT);
    unsafe {
        framebuffer.draw_stage(
            completed as u8,
            petroleum::graphics::boot_screen::KERNEL_STAGE_COUNT,
            label,
        );
    }
}

/// Draw a small status line at the bottom of the boot panel — used as a
/// serial-free progress indicator for init steps on real hardware.
pub fn draw_step_hint(hint: &[u8]) {
    let fb = match crate::graphics::discovery::direct_boot_framebuffer() {
        Some(f) => f,
        None => return,
    };
    let fbw = fb.width();
    let fbh = fb.height();
    let margin = (fbw.min(fbh) / 20).clamp(12, 40);
    let panel_w = fbw.saturating_sub(margin * 2).min(760);
    let panel_h = (if fbh >= 360 { 180 } else { 132 }).min(fbh.saturating_sub(margin * 2));
    let panel_x = (fbw - panel_w) / 2;
    let panel_y = (fbh - panel_h) / 2;
    let y = panel_y + panel_h - 16;
    let x = panel_x + 24;
    unsafe { fb.draw_text(x, y, hint, 1, 0x8c8c96); }
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
