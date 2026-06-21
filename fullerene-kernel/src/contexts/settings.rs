//! SettingsContext — unified runtime settings aggregate.
//!
//! Holds all user-configurable settings that persist across reboots.
//! The context is stored inside [`KernelContext`] and accessed via
//! `kernel_context().with(|k| { let sens = k.settings.mouse.sensitivity; … })`.
//!
//! # Design
//!
//! Individual setting groups (`MouseSettings`, `DisplaySettings`) are
//! plain structs with getter/setter methods.  The GUI settings app only
//! calls high-level setters like `settings.display.set_brightness(0.8)`,
//! and the context takes care of applying changes to the relevant
//! subsystem (framebuffer, input driver, etc.).
//!
//! # Persistence
//!
//! Settings are read from `/etc/settings.toml` at boot and written back
//! whenever a value changes.  The TOML format is:
//!
//! ```toml
//! [mouse]
//! sensitivity = 1.0
//! acceleration = false
//!
//! [display]
//! brightness = 1.0
//! top_panel_enabled = true
//! ```

use core::sync::atomic::{AtomicU32, Ordering};

// ── Mouse settings ─────────────────────────────────────────────

#[derive(Debug)]
pub struct MouseSettings {
    /// Sensitivity multiplier (0.25 .. 4.0, default 1.0).
    /// Stored as fixed-point × 100: 1.0 → 100.
    sensitivity_x100: AtomicU32,
    /// Whether pointer acceleration is enabled.
    acceleration: AtomicU32, // 0 = off, 1 = on (AtomicBool not available)
}

impl MouseSettings {
    pub const fn new() -> Self {
        Self {
            sensitivity_x100: AtomicU32::new(100), // 1.0
            acceleration: AtomicU32::new(0),
        }
    }

    /// Get sensitivity as f32.
    pub fn sensitivity(&self) -> f32 {
        self.sensitivity_x100.load(Ordering::Relaxed) as f32 / 100.0
    }

    /// Set sensitivity (clamped to 0.25 .. 4.0).
    pub fn set_sensitivity(&self, val: f32) {
        let clamped = val.clamp(0.25, 4.0);
        self.sensitivity_x100
            .store((clamped * 100.0) as u32, Ordering::Relaxed);
    }

    /// Get sensitivity as a raw i16 multiplier for the PS/2 driver
    /// (the driver multiplies dx/dy by this value).
    pub fn sensitivity_raw(&self) -> i16 {
        let v = self.sensitivity();
        if v >= 1.0 {
            (v * 6.0) as i16
        } else {
            // Below 1.0: use fixed-point scaling in the driver.
            // The driver currently does `dx.wrapping_mul(self.sensitivity)`,
            // so for values < 1 we return 1 and handle fine scaling elsewhere.
            // For now return 1 and let the f32 path in solvent handle it.
            (v * 6.0) as i16
        }
    }

    pub fn acceleration(&self) -> bool {
        self.acceleration.load(Ordering::Relaxed) != 0
    }

    pub fn set_acceleration(&self, on: bool) {
        self.acceleration.store(if on { 1 } else { 0 }, Ordering::Relaxed);
    }
}

// ── Display settings ──────────────────────────────────────────

#[derive(Debug)]
pub struct DisplaySettings {
    /// Software brightness multiplier (0.1 .. 1.0, default 1.0).
    /// Stored as fixed-point × 100.
    brightness_x100: AtomicU32,
    /// Whether the GNOME-style top panel is visible.
    top_panel_enabled: AtomicU32, // 0 = off, 1 = on
}

impl DisplaySettings {
    pub const fn new() -> Self {
        Self {
            brightness_x100: AtomicU32::new(100), // 1.0
            top_panel_enabled: AtomicU32::new(1), // on by default
        }
    }

    /// Get brightness as f32 (0.1 .. 1.0).
    pub fn brightness(&self) -> f32 {
        self.brightness_x100.load(Ordering::Relaxed) as f32 / 100.0
    }

    /// Set brightness (clamped to 0.1 .. 1.0).
    pub fn set_brightness(&self, val: f32) {
        let clamped = val.clamp(0.1, 1.0);
        self.brightness_x100
            .store((clamped * 100.0) as u32, Ordering::Relaxed);
    }

    pub fn top_panel_enabled(&self) -> bool {
        self.top_panel_enabled.load(Ordering::Relaxed) != 0
    }

    pub fn set_top_panel_enabled(&self, on: bool) {
        self.top_panel_enabled
            .store(if on { 1 } else { 0 }, Ordering::Relaxed);
    }

    pub fn toggle_top_panel(&self) -> bool {
        let prev = self.top_panel_enabled.fetch_xor(1, Ordering::Relaxed);
        prev == 0
    }
}

// ── SettingsContext ────────────────────────────────────────────

/// Aggregate of all user-configurable settings.
///
/// Each sub-group is atomically readable/writable so that the kernel
/// and runtime can read them without locking.
pub struct SettingsContext {
    pub mouse: MouseSettings,
    pub display: DisplaySettings,
}

impl SettingsContext {
    pub const fn new() -> Self {
        Self {
            mouse: MouseSettings::new(),
            display: DisplaySettings::new(),
        }
    }
}