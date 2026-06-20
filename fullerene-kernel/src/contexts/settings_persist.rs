//! Settings persistence: load/save `/etc/settings.toml` via VFS.
//!
//! The format is a minimal TOML subset that we parse manually to avoid
//! pulling in a full TOML crate into the kernel.
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

use alloc::string::String;
use alloc::vec::Vec;

/// Parse a `key = value` line from the TOML file.
fn parse_kv(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
        return None;
    }
    let mut parts = line.splitn(2, '=');
    let key = parts.next()?.trim();
    let value = parts.next()?.trim();
    if key.is_empty() || value.is_empty() {
        return None;
    }
    Some((key, value))
}

fn parse_f32(s: &str) -> Option<f32> {
    let s = s.trim();
    let mut result = 0.0f32;
    let mut decimal = false;
    let mut div = 1.0f32;
    for (i, ch) in s.bytes().enumerate() {
        match ch {
            b'-' if i == 0 => continue,
            b'0'..=b'9' if !decimal => {
                result = result * 10.0 + (ch - b'0') as f32;
            }
            b'0'..=b'9' if decimal => {
                div *= 10.0;
                result += (ch - b'0') as f32 / div;
            }
            b'.' if !decimal => {
                decimal = true;
            }
            _ => return None,
        }
    }
    if s.starts_with('-') {
        result = -result;
    }
    Some(result)
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Load settings from `/etc/settings.toml` via VFS and apply them.
///
/// Returns `(sensitivity, brightness_x100, top_panel_enabled)` so the
/// caller can sync to solvent.
pub fn load_settings(
    read_fn: impl FnOnce(&str) -> Result<Vec<u8>, &'static str>,
) -> (f32, u32, bool) {
    let data = match read_fn("/etc/settings.toml") {
        Ok(data) => data,
        Err(_) => return (1.0, 100, true),
    };

    let text = match core::str::from_utf8(&data) {
        Ok(s) => s,
        Err(_) => return (1.0, 100, true),
    };

    let mut section: Option<&str> = None;
    let mut sensitivity: f32 = 1.0;
    let mut brightness: f32 = 1.0;
    let mut top_panel: bool = true;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = Some(&trimmed[1..trimmed.len() - 1]);
            continue;
        }
        if let Some((key, value)) = parse_kv(trimmed) {
            match (section, key) {
                (Some("mouse"), "sensitivity") => {
                    if let Some(v) = parse_f32(value) {
                        sensitivity = v;
                    }
                }
                (Some("display"), "brightness") => {
                    if let Some(v) = parse_f32(value) {
                        brightness = v;
                    }
                }
                (Some("display"), "top_panel_enabled") => {
                    if let Some(v) = parse_bool(value) {
                        top_panel = v;
                    }
                }
                _ => {}
            }
        }
    }

    let bright_x100 = (brightness.clamp(0.1, 1.0) * 100.0) as u32;
    (sensitivity.clamp(0.25, 4.0), bright_x100, top_panel)
}

/// Build a TOML string from current settings.
pub fn format_settings_toml(sensitivity: f32, brightness_x100: u32, top_panel: bool) -> String {
    alloc::format!(
        "# Fullerene Settings\n\
         # Auto-generated — do not edit while the system is running\n\
         \n\
         [mouse]\n\
         sensitivity = {}\n\
         acceleration = false\n\
         \n\
         [display]\n\
         brightness = {:.2}\n\
         top_panel_enabled = {}\n",
        sensitivity,
        brightness_x100 as f32 / 100.0,
        top_panel,
    )
}