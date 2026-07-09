//! Clock / wall-clock update and timezone management.
//!
//! Extracted from `lib.rs` to reduce the size of the god-module.

use crate::{RUNTIME, SOLVENT_CALLBACKS};
use alloc::string::String;
use spin::Mutex;

/// Timezone offset in hours (positive = east of UTC).
pub(crate) static TIMEZONE_OFFSET_HOURS: core::sync::atomic::AtomicI8 =
    core::sync::atomic::AtomicI8::new(9);

/// Cached wall-clock string (read by the desktop taskbar / top panel).
static CLOCK_STRING: Mutex<String> = Mutex::new(String::new());

pub fn clock_string() -> String {
    CLOCK_STRING.lock().clone()
}

// ── Days per month (non‑leap) ─────────────────────────────────

const DAYS_IN_MONTH: [i16; 13] = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
fn days_in_month(month: i16, year: i16) -> i16 {
    if month == 2 && ((year % 4 == 0 && year % 100 != 0) || year % 400 == 0) {
        29
    } else if (1..=12).contains(&month) {
        DAYS_IN_MONTH[month as usize]
    } else {
        31
    }
}

/// Read the RTC and produce a human-readable time string.
///
/// Timezone offset is applied here so the user can change it at runtime.
/// Returns `true` when the string actually changed (caller should push a
/// dirty rect for the taskbar / top panel).
pub fn update_clock() -> bool {
    let offset = TIMEZONE_OFFSET_HOURS.load(core::sync::atomic::Ordering::Relaxed);
    let time_str = if let Some(get_time) = SOLVENT_CALLBACKS.lock().wall_clock {
        if let Some((year, month, day, hour, minute, _second)) = get_time() {
            let mut local_hour = hour as i16 + offset as i16;
            let mut local_day = day as i16;
            let mut local_month = month as i16;
            let mut local_year = year as i16;
            while local_hour < 0 {
                local_hour += 24;
                local_day -= 1;
            }
            while local_hour >= 24 {
                local_hour -= 24;
                local_day += 1;
            }
            if local_day > days_in_month(local_month, local_year) {
                local_day = 1;
                local_month += 1;
                if local_month > 12 {
                    local_month = 1;
                    local_year += 1;
                }
            } else if local_day < 1 {
                local_month -= 1;
                if local_month < 1 {
                    local_month = 12;
                    local_year -= 1;
                }
                local_day = days_in_month(local_month, local_year) + local_day;
            }
            alloc::format!(
                "{} {:02}{:02} {:02}{:02}",
                local_year as u16, local_month as u8, local_day as u8, local_hour as u8, minute
            )
        } else {
            String::from("---- ---- ----")
        }
    } else {
        String::from("---- ---- ----")
    };

    let mut rt = crate::RUNTIME.lock();
    let mut changed = false;
    if let Some(ref mut r) = *rt {
        if r.desktop.clock_text != time_str {
            r.clock_changed = true;
            r.desktop.clock_text = time_str.clone();
            r.desktop.top_panel.clock_text = time_str.clone();
            changed = true;
        }
    }
    *CLOCK_STRING.lock() = time_str;
    changed
}