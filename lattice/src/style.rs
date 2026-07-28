//! Runtime selection of a complete Lattice desktop implementation.
//!
//! This is intentionally enum/atomic dispatch rather than boxed trait
//! objects.  The compositor is used in `no_std` boot paths where avoiding a
//! heap allocation for a style switch is useful.

use crate::basalt;
use crate::common::{LatticeStyle, ShellKind, StyleSpec};
use crate::photon;
use crate::prism;
use core::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum LatticeVariant {
    Basalt = 0,
    Photon = 1,
    Prism = 2,
}

impl LatticeVariant {
    pub const fn from_u32(value: u32) -> Self {
        match value {
            1 => Self::Photon,
            2 => Self::Prism,
            _ => Self::Basalt,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Basalt => "Basalt",
            Self::Photon => "Photon",
            Self::Prism => "Prism",
        }
    }

    pub const fn next(self, forward: bool) -> Self {
        match (self, forward) {
            (Self::Basalt, true) => Self::Photon,
            (Self::Photon, true) => Self::Prism,
            (Self::Prism, true) => Self::Basalt,
            (Self::Basalt, false) => Self::Prism,
            (Self::Photon, false) => Self::Basalt,
            (Self::Prism, false) => Self::Photon,
        }
    }
}

static ACTIVE_VARIANT: AtomicU32 = AtomicU32::new(LatticeVariant::Basalt as u32);

pub fn variant() -> LatticeVariant {
    LatticeVariant::from_u32(ACTIVE_VARIANT.load(Ordering::Acquire))
}

pub fn set_variant(value: LatticeVariant) {
    ACTIVE_VARIANT.store(value as u32, Ordering::Release);
}

pub fn style_for(value: LatticeVariant) -> &'static dyn LatticeStyle {
    match value {
        LatticeVariant::Basalt => basalt::style(),
        LatticeVariant::Photon => photon::style(),
        LatticeVariant::Prism => prism::style(),
    }
}

pub fn current() -> &'static StyleSpec {
    style_for(variant()).spec()
}

pub fn kind() -> ShellKind {
    current().kind
}

pub fn title_bar_height() -> u32 {
    current().metrics.title_bar_height
}

pub fn taskbar_height() -> u32 {
    current().metrics.taskbar_height
}

pub fn top_panel_height() -> u32 {
    current().metrics.top_panel_height
}

pub fn window_radius() -> u32 {
    // Zero remains the explicit square-corner setting.  Any non-zero value
    // selects the variant's designed radius.
    let override_radius = crate::compositor::WINDOW_CORNER_RADIUS.load(Ordering::Relaxed);
    if override_radius == 0 {
        0
    } else {
        current().metrics.window_radius
    }
}

pub fn title_button_x(window_x: i32, window_width: u32, button: u32) -> i32 {
    if kind() == ShellKind::Photon {
        window_x + window_width as i32 - 22 - button as i32 * 20
    } else if current().metrics.title_buttons_on_left {
        window_x + 12 + button as i32 * 20
    } else {
        // Keep the historical hit-test geometry used by Basalt.  The close
        // glyph has a small overhang on very narrow windows, which is also
        // how the original shell avoided stealing title-bar drag gestures.
        window_x + window_width as i32 - 18 - button as i32 * 20
    }
}

pub fn title_text_x(window_x: i32) -> i32 {
    if current().metrics.title_buttons_on_left {
        window_x + 80
    } else {
        window_x + 12
    }
}

/// Return the rendered task entry rectangle for a style.
pub fn taskbar_entry_rect(
    index: usize,
    count: usize,
    fb_width: u32,
    fb_height: u32,
) -> Option<(i32, i32, u32, u32)> {
    match kind() {
        ShellKind::Basalt => Some((
            4 + index as i32 * 124,
            fb_height.saturating_sub(taskbar_height()) as i32 + 3,
            120,
            taskbar_height().saturating_sub(6),
        )),
        ShellKind::Photon => {
            let dock_x = 16;
            Some((
                dock_x as i32 + 12 + (crate::common::PHOTON_LAUNCHER_COUNT + index) as i32 * 48,
                fb_height.saturating_sub(taskbar_height()) as i32 + 10,
                40,
                40,
            ))
        }
        ShellKind::Prism => {
            let item_width = 52u32;
            let total = (count + crate::common::PRISM_LAUNCHER_COUNT) as u32 * item_width;
            let start = fb_width.saturating_sub(total) / 2;
            Some((
                start as i32
                    + (crate::common::PRISM_LAUNCHER_COUNT + index) as i32 * item_width as i32
                    + 4,
                fb_height.saturating_sub(taskbar_height()) as i32 + 8,
                44,
                36,
            ))
        }
    }
}

/// Return a static launcher slot for styles that present a persistent dock or
/// centred launcher cluster. Window entries use [`taskbar_entry_rect`].
pub fn launcher_entry_rect(
    index: usize,
    count: usize,
    fb_width: u32,
    fb_height: u32,
) -> Option<(i32, i32, u32, u32)> {
    match kind() {
        ShellKind::Photon if index < crate::common::PHOTON_LAUNCHER_COUNT => Some((
            16 + 12 + index as i32 * 48,
            fb_height.saturating_sub(taskbar_height()) as i32 + 10,
            40,
            40,
        )),
        ShellKind::Prism if index < crate::common::PRISM_LAUNCHER_COUNT => {
            let item_width = 52u32;
            let total = (count + crate::common::PRISM_LAUNCHER_COUNT) as u32 * item_width;
            let start = fb_width.saturating_sub(total) / 2;
            Some((
                start as i32 + index as i32 * item_width as i32 + 4,
                fb_height.saturating_sub(taskbar_height()) as i32 + 8,
                44,
                36,
            ))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_have_independent_shell_metrics() {
        let basalt = style_for(LatticeVariant::Basalt).spec();
        let photon = style_for(LatticeVariant::Photon).spec();
        let prism = style_for(LatticeVariant::Prism).spec();

        assert_ne!(basalt.metrics.taskbar_height, photon.metrics.taskbar_height);
        assert_ne!(
            photon.metrics.title_bar_height,
            prism.metrics.title_bar_height
        );
        assert_ne!(basalt.palette.bg, photon.palette.bg);
        assert_eq!(LatticeVariant::Basalt.next(true), LatticeVariant::Photon);
        assert_eq!(LatticeVariant::Photon.next(true), LatticeVariant::Prism);
        assert_eq!(LatticeVariant::Prism.next(true), LatticeVariant::Basalt);
    }
}
