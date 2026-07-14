//! Theme system for Lattice compositor.
//!
//! Two style axes:
//!   **Style**: Classic or Modern (visual appearance)
//!   **Variant**: Dark or Light (brightness)
//!
//! All colour constants are stored here and consumed by the compositor,
//! taskbar, and shell overlay renderers.

use core::sync::atomic::{AtomicBool, Ordering};

/// Visual style (appearance family).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeStyle {
    Classic,
    Modern,
}

/// Brightness variant within a style.
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

// ── Classic style ──────────────────────────────────────────────

/// Classic dark theme (original Fullerene look).
pub const CLASSIC_DARK_THEME: ThemeColors = ThemeColors {
    bg: 0x1B1B1D,
    surface: 0x242426,
    primary: 0x3584E4,
    active: 0x2A7DE0,
    text: 0xE0E0E0,
    muted: 0x888888,
    border_active: 0x3584E4,
    border_inactive: 0x555555,
    title_active: 0x2A2A2C,
    title_inactive: 0x333335,
    accent: 0xE6A817,
    danger: 0xD94A4A,
    taskbar_bg: 0x151516,
    taskbar_text: 0xCCCCCC,
    taskbar_active_bg: 0x3584E4,
    taskbar_inactive_bg: 0x2C2C2E,
};

/// Classic light theme.
pub const CLASSIC_LIGHT_THEME: ThemeColors = ThemeColors {
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

// ── Modern style ───────────────────────────────────────────────

/// Modern dark theme — macOS/iOS‑inspired flat design.
pub const MODERN_DARK_THEME: ThemeColors = ThemeColors {
    bg: 0x1B1B1D,
    surface: 0x242426,
    primary: 0x3584E4,
    active: 0x2A7DE0,
    text: 0xF5F5F5,
    muted: 0x98989A,
    border_active: 0x3584E4,
    border_inactive: 0x3A3A3C,
    title_active: 0x1C1C1E,
    title_inactive: 0x2A2A2C,
    accent: 0xFF9F0A,
    danger: 0xFF3B30,
    taskbar_bg: 0x151516,
    taskbar_text: 0xF5F5F5,
    taskbar_active_bg: 0x3584E4,
    taskbar_inactive_bg: 0x2C2C2E,
};

/// Modern light theme — clean white with blue accent.
pub const MODERN_LIGHT_THEME: ThemeColors = ThemeColors {
    bg: 0xFFFFFF,
    surface: 0xF2F2F7,
    primary: 0x007AFF,
    active: 0x0066D6,
    text: 0x1C1C1E,
    muted: 0x8E8E93,
    border_active: 0x007AFF,
    border_inactive: 0xC7C7CC,
    title_active: 0x007AFF,
    title_inactive: 0xAEAEB2,
    accent: 0xFF9F0A,
    danger: 0xFF3B30,
    taskbar_bg: 0xE5E5EA,
    taskbar_text: 0x1C1C1E,
    taskbar_active_bg: 0x007AFF,
    taskbar_inactive_bg: 0xC7C7CC,
};

// ── Global state ───────────────────────────────────────────────

/// false = Classic, true = Modern
static STYLE_SEL: AtomicBool = AtomicBool::new(true);

/// false = Dark, true = Light
static VARIANT_SEL: AtomicBool = AtomicBool::new(false);

/// Get the currently active style.
pub fn current_style() -> ThemeStyle {
    if STYLE_SEL.load(Ordering::SeqCst) {
        ThemeStyle::Modern
    } else {
        ThemeStyle::Classic
    }
}

/// Get the currently active brightness variant.
pub fn current_theme_variant() -> ThemeVariant {
    if VARIANT_SEL.load(Ordering::SeqCst) {
        ThemeVariant::Light
    } else {
        ThemeVariant::Dark
    }
}

/// Get the currently active theme colours.
pub fn current_colors() -> ThemeColors {
    let style = current_style();
    let variant = current_theme_variant();
    match (style, variant) {
        (ThemeStyle::Classic, ThemeVariant::Dark) => CLASSIC_DARK_THEME,
        (ThemeStyle::Classic, ThemeVariant::Light) => CLASSIC_LIGHT_THEME,
        (ThemeStyle::Modern, ThemeVariant::Dark) => MODERN_DARK_THEME,
        (ThemeStyle::Modern, ThemeVariant::Light) => MODERN_LIGHT_THEME,
    }
}

// ── Style switching ────────────────────────────────────────────

/// Toggle between Classic and Modern style.
pub fn toggle_style() -> ThemeStyle {
    let was_modern = STYLE_SEL.fetch_xor(true, Ordering::SeqCst);
    if was_modern { ThemeStyle::Classic } else { ThemeStyle::Modern }
}

/// Set the style explicitly.
pub fn set_style(style: ThemeStyle) {
    STYLE_SEL.store(matches!(style, ThemeStyle::Modern), Ordering::SeqCst);
}

/// Toggle between dark and light variant.
pub fn toggle_theme() -> ThemeVariant {
    let was_light = VARIANT_SEL.fetch_xor(true, Ordering::SeqCst);
    if was_light { ThemeVariant::Dark } else { ThemeVariant::Light }
}

/// Set the variant explicitly.
pub fn set_theme(variant: ThemeVariant) {
    VARIANT_SEL.store(matches!(variant, ThemeVariant::Light), Ordering::SeqCst);
}

// ── Colour lookups (shell / settings) ──────────────────────────

/// Get a single colour value by name.
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
        "bg", "surface", "primary", "active", "text", "muted",
        "border_active", "border_inactive", "title_active", "title_inactive",
        "accent", "danger",
        "taskbar_bg", "taskbar_text", "taskbar_active_bg", "taskbar_inactive_bg",
    ]
}
