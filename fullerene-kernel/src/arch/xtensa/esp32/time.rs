//! ESP32 timer and monotonic time.

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
const TIMER_DIVIDER: u32 = 1 << 13;
const TIMER_LEVEL_INT_ENABLE: u32 = 1 << 11;
const TIMER_ALARM_ENABLE: u32 = 1 << 10;

static mut TICKS: u64 = 0;

pub fn init() {
    // Count from zero to 79_999 at the 80 MHz APB clock, then reload.
    super::platform::write_register(TIMER0_BASE + TIMER_LOAD_VALUE, 0);
    super::platform::write_register(TIMER0_BASE + TIMER_LOAD_VALUE_HIGH, 0);
    super::platform::write_register(TIMER0_BASE + TIMER_ALARM, 80_000 - 1);
    super::platform::write_register(TIMER0_BASE + TIMER_ALARM_HIGH, 0);
    super::platform::write_register(TIMER0_BASE + TIMER_LOAD, 1);
    super::platform::write_register(
        TIMER0_BASE + TIMER_CONTROL,
        TIMER_ENABLE
            | TIMER_INCREASE
            | TIMER_AUTORELOAD
            | TIMER_DIVIDER
            | TIMER_LEVEL_INT_ENABLE
            | TIMER_ALARM_ENABLE,
    );
    super::platform::write_register(TIMER0_BASE + TIMER_INT_ENABLE, 1);
    super::interrupts::enable_timer_interrupt();
}

#[inline]
pub fn raw_ticks() -> u64 {
    unsafe { TICKS + u64::from(read_register(TIMER0_BASE + TIMER_COUNT)) }
}

pub fn uptime_micros() -> u64 {
    raw_ticks()
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

#[unsafe(no_mangle)]
pub extern "C" fn timer0_interrupt() {
    unsafe {
        TICKS = TICKS.wrapping_add(80_000);
    }
    crate::arch::xtensa::esp32::scheduler::request_yield();
    super::platform::write_register(TIMER0_BASE + TIMER_INT_CLEAR, 1);
}
