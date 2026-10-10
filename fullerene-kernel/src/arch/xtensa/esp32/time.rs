//! ESP32 timer and monotonic time.
//!
//! The ESP32 general-purpose timer counts `APB_CLK / prescale`. ESP-IDF's
//! `timer_ll_set_clock_prescale` documents the prescale field as "0 and 1 are
//! not valid" and hands the value straight to the register, so a prescale of 80
//! is the documented way to get a 1 MHz tick out of the 80 MHz APB clock.
//!
//! This module used to write a prescale field of 1 - below the documented
//! minimum - while adding 80_000 ticks per interrupt, which only lines up if
//! the timer happens to run at 80 MHz. Since the hardware is not documented to
//! produce that rate, `uptime_micros` and `sleep_ticks` were both off by a
//! large factor. The configuration below makes the unit explicit instead: one
//! tick is one microsecond by construction, and the alarm reloads every
//! millisecond.

use super::platform::{TIMER0_BASE, read_register};

const TIMER_CONTROL: usize = 0x00; // TIMG_T0CONFIG_REG
const TIMER_COUNT: usize = 0x04;
const TIMER_ALARM: usize = 0x10; // TIMG_T0ALARMLO_REG
const TIMER_ALARM_HIGH: usize = 0x14; // TIMG_T0ALARMHI_REG
const TIMER_LOAD_VALUE: usize = 0x18; // TIMG_T0LOADLO_REG
const TIMER_LOAD_VALUE_HIGH: usize = 0x1c; // TIMG_T0LOADHI_REG
const TIMER_LOAD: usize = 0x20; // TIMG_T0LOAD_REG
const TIMER_INT_ENABLE: usize = 0x98; // TIMG_INT_ENA_TIMERS_REG
const TIMER_INT_CLEAR: usize = 0xa4; // TIMG_INT_CLR_TIMERS_REG

const TIMER_ENABLE: u32 = 1 << 31;
const TIMER_INCREASE: u32 = 1 << 30;
const TIMER_AUTORELOAD: u32 = 1 << 29;
const TIMER_LEVEL_INT_ENABLE: u32 = 1 << 11;
const TIMER_ALARM_ENABLE: u32 = 1 << 10;

/// The APB clock the timer divides.
const APB_CLOCK_HZ: u32 = 80_000_000;
/// Prescale for a 1 MHz count, i.e. one tick per microsecond.
const TIMER_PRESCALE: u32 = APB_CLOCK_HZ / 1_000_000;
/// The prescale occupies bits [28:13] of `TIMG_T0CONFIG_REG`.
const TIMER_DIVIDER_SHIFT: u32 = 13;
/// Alarm value for a 1 ms period at 1 MHz.
const ALARM_TICKS: u32 = 1_000;
/// Microseconds added per alarm interrupt.
const TICKS_PER_INTERRUPT: u64 = 1_000;

static mut TICKS: u64 = 0;

pub fn init() {
    // Count from zero to 999 at 1 MHz, then reload: one interrupt per
    // millisecond.
    super::platform::write_register(TIMER0_BASE + TIMER_LOAD_VALUE, 0);
    super::platform::write_register(TIMER0_BASE + TIMER_LOAD_VALUE_HIGH, 0);
    super::platform::write_register(TIMER0_BASE + TIMER_ALARM, ALARM_TICKS - 1);
    super::platform::write_register(TIMER0_BASE + TIMER_ALARM_HIGH, 0);
    super::platform::write_register(TIMER0_BASE + TIMER_LOAD, 1);
    super::platform::write_register(
        TIMER0_BASE + TIMER_CONTROL,
        TIMER_ENABLE
            | TIMER_INCREASE
            | TIMER_AUTORELOAD
            | (TIMER_PRESCALE << TIMER_DIVIDER_SHIFT)
            | TIMER_LEVEL_INT_ENABLE
            | TIMER_ALARM_ENABLE,
    );
    super::platform::write_register(TIMER0_BASE + TIMER_INT_ENABLE, 1);
    super::interrupts::enable_timer_interrupt();
}

/// The timer's resolution, so callers can state their units instead of
/// assuming them.
pub const TICKS_PER_MICROSECOND: u64 = 1;

/// Microseconds since boot.
#[inline]
pub fn raw_ticks() -> u64 {
    unsafe { TICKS + u64::from(read_register(TIMER0_BASE + TIMER_COUNT)) }
}

pub fn uptime_micros() -> u64 {
    raw_ticks() / TICKS_PER_MICROSECOND
}

pub fn uptime_millis() -> u64 {
    uptime_micros() / 1_000
}

pub fn sleep_ticks(ticks: u32) {
    let deadline = raw_ticks() + u64::from(ticks);
    while raw_ticks() < deadline {
        core::hint::spin_loop();
    }
}

/// The prescaler this module programs, exposed so the latency report can state
/// the timebase it measured against.
pub const fn prescale() -> u32 {
    TIMER_PRESCALE
}

#[unsafe(no_mangle)]
pub extern "C" fn timer0_interrupt() {
    unsafe {
        TICKS = TICKS.wrapping_add(TICKS_PER_INTERRUPT);
    }
    crate::arch::xtensa::esp32::scheduler::request_yield();
    super::platform::write_register(TIMER0_BASE + TIMER_INT_CLEAR, 1);
}

#[cfg(test)]
mod tests {
    #[test]
    fn one_tick_is_one_microsecond_by_construction() {
        // The prescale divides the 80 MHz APB clock down to the tick rate, and
        // the alarm reloads at a whole millisecond.
        assert_eq!(super::TIMER_PRESCALE, 80);
        assert_eq!(super::ALARM_TICKS, 1_000);
        assert_eq!(super::TICKS_PER_INTERRUPT, 1_000);
        assert_eq!(80_000_000 / super::TIMER_PRESCALE, 1_000_000);
    }
}
