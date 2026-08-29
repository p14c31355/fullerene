//! ESP32 timer and monotonic time.

use super::platform::{TIMER0_BASE, read_register};

const TIMER_LOAD: usize = 0x00;
const TIMER_COUNT: usize = 0x04;
const TIMER_CONTROL: usize = 0x08;
const TIMER_INT_CLEAR: usize = 0x0c;

static mut TICKS: u64 = 0;

pub fn init() {
    super::platform::write_register(TIMER0_BASE + TIMER_LOAD, 80_000 - 1);
    super::platform::write_register(TIMER0_BASE + TIMER_CONTROL, 0x01c);
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
