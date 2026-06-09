//! Clock utilities for Toluene SDK.
//!
//! Provides time formatting and a simple time structure
//! for clock applications.

/// Time representation (local time).
#[derive(Debug, Clone, Copy, Default)]
pub struct Time {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl Time {
    /// Format time as "YYYY MMDD HHMM" (compact form used by taskbar).
    pub fn format_compact(&self) -> [u8; 14] {
        let mut buf = [b' '; 14];
        Self::write_u16(&mut buf[0..4], self.year);
        buf[4] = b' ';
        Self::write_u8_pad(&mut buf[5..7], self.month);
        Self::write_u8_pad(&mut buf[7..9], self.day);
        buf[9] = b' ';
        Self::write_u8_pad(&mut buf[10..12], self.hour);
        Self::write_u8_pad(&mut buf[12..14], self.minute);
        buf
    }

    /// Format time as "HH:MM:SS".
    pub fn format_time(&self) -> [u8; 8] {
        let mut buf = [b'0'; 8];
        Self::write_u8_pad(&mut buf[0..2], self.hour);
        buf[2] = b':';
        Self::write_u8_pad(&mut buf[3..5], self.minute);
        buf[5] = b':';
        Self::write_u8_pad(&mut buf[6..8], self.second);
        buf
    }

    /// Format date as "YYYY-MM-DD".
    pub fn format_date(&self) -> [u8; 10] {
        let mut buf = [b'0'; 10];
        Self::write_u16(&mut buf[0..4], self.year);
        buf[4] = b'-';
        Self::write_u8_pad(&mut buf[5..7], self.month);
        buf[7] = b'-';
        Self::write_u8_pad(&mut buf[8..10], self.day);
        buf
    }

    /// Ticks since midnight in seconds.
    pub fn seconds_since_midnight(&self) -> u32 {
        self.hour as u32 * 3600 + self.minute as u32 * 60 + self.second as u32
    }

    fn write_u8_pad(dst: &mut [u8], val: u8) {
        let v = val.min(99);
        dst[0] = b'0' + (v / 10);
        dst[1] = b'0' + (v % 10);
    }

    fn write_u16(dst: &mut [u8], val: u16) {
        let v = val.min(9999);
        dst[0] = b'0' + ((v / 1000) % 10) as u8;
        dst[1] = b'0' + ((v / 100) % 10) as u8;
        dst[2] = b'0' + ((v / 10) % 10) as u8;
        dst[3] = b'0' + (v % 10) as u8;
    }
}

/// Days in each month (non-leap).
const DAYS_IN_MONTH: &[u8] = &[31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// Check if a year is a leap year.
pub fn is_leap_year(y: u16) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

/// Get days in a month.
pub fn days_in_month(month: u8, year: u16) -> u8 {
    if month == 2 && is_leap_year(year) {
        29
    } else if month >= 1 && month <= 12 {
        DAYS_IN_MONTH[(month - 1) as usize]
    } else {
        31
    }
}
