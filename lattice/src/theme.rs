//! Theme system for Lattice compositor.
//!
//! Provides dark and light theme variants with runtime switching.
//! All colour constants are stored here and consumed by the compositor,
//! taskbar, and shell overlay renderers.

/// Available theme variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeVariant {
    Dark,
    Light,
}

/// Full colour palette for a theme.
#[derive(Debug, Clone, Copy)]
pub struct ThemeColors {
    pub bg: u32,
    pub surface: u32,
    pub primary: u32,
    pub active: u32,
    pub text: u32,
    pub muted: u32,
    pub border_active: u32,
    pub border_inactive: u32,
    pub title_active: u32,
    pub title_inactive: u32,
    pub accent: u32,
    pub danger: u32,
    pub taskbar_bg: u32,
    pub taskbar_text: u32,
    pub taskbar_active_bg: u32,
    pub taskbar_inactive_bg: u32,
}

/// Fullerene dark theme (default).
pub const DARK_THEME: ThemeColors = ThemeColors {
    bg: 0x1a1a2e,
    surface: 0x16213e,
    primary: 0x4A90D9,
    active: 0x3A7BD5,
    text: 0xE0E0E0,
    muted: 0x888888,
    border_active: 0x4A90D9,
    border_inactive: 0x555555,
    title_active: 0x3A7BD5,
    title_inactive: 0x444444,
    accent: 0xE6A817,
    danger: 0xD94A4A,
    taskbar_bg: 0x0F0F1A,
    taskbar_text: 0xCCCCCC,
    taskbar_active_bg: 0x3A7BD5,
    taskbar_inactive_bg: 0x333344,
};

/// Fullerene light theme.
pub const LIGHT_THEME: ThemeColors = ThemeColors {
    bg: 0xF0F0F5,
    surface: 0xFFFFFF,
    primary: 0x2563EB,
    active: 0x1D4ED8,
    text: 0x1A1A2E,
    muted: 0x6B7280,
    border_active: 0x2563EB,
    border_inactive: 0x9CA3AF,
    title_active: 0x1D4ED8,
    title_inactive: 0x9CA3AF,
    accent: 0xD97706,
    danger: 0xDC2626,
    taskbar_bg: 0xE5E7EB,
    taskbar_text: 0x374151,
    taskbar_active_bg: 0x2563EB,
    taskbar_inactive_bg: 0x9CA3AF,
};

/// Global theme state, toggleable at runtime.
use core::sync::atomic::{AtomicBool, Ordering};

/// false = Dark, true = Light
static CURRENT_THEME: AtomicBool = AtomicBool::new(false);

/// Get the currently active theme variant.
pub fn current_theme_variant() -> ThemeVariant {
    if CURRENT_THEME.load(Ordering::SeqCst) {
        ThemeVariant::Light
    } else {
        ThemeVariant::Dark
    }
}

/// Get the currently active theme colours.
pub fn current_colors() -> ThemeColors {
    if CURRENT_THEME.load(Ordering::SeqCst) {
        LIGHT_THEME
    } else {
        DARK_THEME
    }
}

/// Toggle between dark and light theme.
pub fn toggle_theme() -> ThemeVariant {
    let was_dark = CURRENT_THEME.swap(true, Ordering::SeqCst);
    // was_dark == false means it was Dark, now switched to Light
    // was_dark == true means it was Light, now switched to Dark
    if was_dark {
        // Was Light → now Dark
        CURRENT_THEME.store(false, Ordering::SeqCst);
        ThemeVariant::Dark
    } else {
        ThemeVariant::Light
    }
}

/// Set the theme explicitly.
pub fn set_theme(variant: ThemeVariant) {
    CURRENT_THEME.store(
        matches!(variant, ThemeVariant::Light),
        Ordering::SeqCst,
    );
}

/// Get a single colour value by name (for shell / settings app).
pub fn get_color(name: &str) -> Option<u32> {
    let c = current_colors();
    match name {
        "bg" => Some(c.bg),
        "surface" => Some(c.surface),
        "primary" => Some(c.primary),
        "active" => Some(c.active),
        "text" => Some(c.text),
        "muted" => Some(c.muted),
        "border_active" => Some(c.border_active),
        "border_inactive" => Some(c.border_inactive),
        "title_active" => Some(c.title_active),
        "title_inactive" => Some(c.title_inactive),
        "accent" => Some(c.accent),
        "danger" => Some(c.danger),
        "taskbar_bg" => Some(c.taskbar_bg),
        "taskbar_text" => Some(c.taskbar_text),
        "taskbar_active_bg" => Some(c.taskbar_active_bg),
        "taskbar_inactive_bg" => Some(c.taskbar_inactive_bg),
        _ => None,
    }
}

/// List all available colour names.
pub fn color_names() -> &'static [&'static str] {
    &[
        "bg",
        "surface",
        "primary",
        "active",
        "text",
        "muted",
        "border_active",
        "border_inactive",
        "title_active",
        "title_inactive",
        "accent",
        "danger",
        "taskbar_bg",
        "taskbar_text",
        "taskbar_active_bg",
        "taskbar_inactive_bg",
    ]
}
