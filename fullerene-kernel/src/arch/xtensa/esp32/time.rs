//! ESP32 timer and monotonic time.
//!
//! The ESP32 general-purpose timer counts `APB_CLK / prescale`. ESP-IDF's
//! `timer_ll_set_clock_prescale` documents the prescale field as "0 and 1 are
//! not valid" and hands the value straight to the register, so a prescale of 80
//! is the documented way to get a 1 MHz tick out of the 80 MHz APB clock.
//! That makes one tick one microsecond by construction, which is what the
//! drawing-latency measurements are stated in.
//!
//! The counter free-runs and **no timer interrupt is enabled**. The port's
//! interrupt dispatch is a separate bring-up item (the ESP32 notes list timer
//! preemption as unfinished), and on hardware, enabling the timer interrupt
//! stops the boot right here: the interrupt fires ~1 ms after the enable while
//! the scheduler is still being constructed. A free-running counter is all the
//! latency work needs, so the interrupt stays off until the vector path exists.
//!
//! This module previously wrote a prescale field of 1 - below the documented
//! minimum - and accounted 80_000 ticks per interrupt, which only lines up if
//! the timer ran at 80 MHz; neither reading is documented, so the unit was
//! undefined.

use super::platform::{TIMER0_BASE, read_register};

const TIMER_CONTROL: usize = 0x00; // TIMG_T0CONFIG_REG
const TIMER_COUNT: usize = 0x04;
const TIMER_LOAD_VALUE: usize = 0x18; // TIMG_T0LOADLO_REG
const TIMER_LOAD_VALUE_HIGH: usize = 0x1c; // TIMG_T0LOADHI_REG
const TIMER_LOAD: usize = 0x20; // TIMG_T0LOAD_REG
/// `TIMG_T0UPDATE_REG`. Writing one is a soft capture: the counter is latched
/// into LO/HI on that write, and the register lives in a different clock domain
/// from the timer logic, so the bit must clear before the value is valid. The
/// reference implementation (`timer_ll_trigger_soft_capture`) waits exactly
/// that way. Without it, LO/HI keep returning the last latched value - which is
/// why a running timer read as a constant zero.
const TIMER_UPDATE: usize = 0x0c;
const TIMER_ALARM_LOW: usize = 0x10; // TIMG_T0ALARMLO_REG
const TIMER_INT_CLEAR: usize = 0xa4; // TIMG_INT_CLR_TIMERS_REG

const TIMER_ENABLE: u32 = 1 << 31;
const TIMER_INCREASE: u32 = 1 << 30;

/// `DPORT_PERIP_CLK_EN_REG` / `DPORT_PERIP_RST_EN_REG`. The timer registers
/// are unreachable until the peripheral's bus clock is enabled; its gate is
/// `DPORT_TIMERGROUP_CLK_EN` (bit 13) in the enable register and
/// `DPORT_TIMERGROUP_RST` (bit 13) in the reset register, per `dport_reg.h`.
const DPORT_PERIP_CLK_EN: usize = 0x3ff0_00c0;
const DPORT_PERIP_RST_EN: usize = 0x3ff0_00c4;
const DPORT_TIMERGROUP_CLK_EN: u32 = 1 << 13;
const DPORT_TIMERGROUP_RST: u32 = 1 << 13;

/// The APB clock the timer divides.
const APB_CLOCK_HZ: u32 = 80_000_000;
/// Prescale for a 1 MHz count, i.e. one tick per microsecond.
const TIMER_PRESCALE: u32 = APB_CLOCK_HZ / 1_000_000;
/// The prescale occupies bits [28:13] of `TIMG_T0CONFIG_REG`.
const TIMER_DIVIDER_SHIFT: u32 = 13;

/// High-order microseconds accumulated across counter wraps.
///
/// The cooperative scheduler runs one task at a time, so these plain statics
/// have a single logical accessor at any moment; there is no second core
/// running this code.
static mut WRAP_BASE: u64 = 0;
static mut LAST_COUNT: u32 = 0;

pub fn init() {
    // Own the peripheral's bus clock before writing any of its registers.
    // Without this the timer reads back a constant and every measured
    // duration is zero.
    super::platform::modify_register(DPORT_PERIP_CLK_EN, 0, DPORT_TIMERGROUP_CLK_EN);
    super::platform::modify_register(DPORT_PERIP_RST_EN, DPORT_TIMERGROUP_RST, 0);

    // Free-running at 1 MHz, never reloading: the whole 32-bit count is
    // available to software, and wraps are handled in `uptime_micros`.
    super::platform::write_register(TIMER0_BASE + TIMER_LOAD_VALUE, 0);
    super::platform::write_register(TIMER0_BASE + TIMER_LOAD_VALUE_HIGH, 0);
    super::platform::write_register(TIMER0_BASE + TIMER_LOAD, 0);
    super::platform::write_register(
        TIMER0_BASE + TIMER_CONTROL,
        TIMER_ENABLE | TIMER_INCREASE | (TIMER_PRESCALE << TIMER_DIVIDER_SHIFT),
    );
}

/// The timer's resolution, so callers can state their units instead of
/// assuming them.
pub const TICKS_PER_MICROSECOND: u64 = 1;

/// Microseconds per tick, for the latency report.
pub const fn tick_micros() -> u32 {
    (1_000 / TICKS_PER_MICROSECOND) as u32
}

/// The CPU clock the cycle counter runs at.
///
/// Reported by the chip itself when the image is written ("240MHz" in the
/// flash stub's chip description); durations derived from cycles are stated in
/// these units and the report prints the constant so the assumption is visible
/// rather than implied.
pub const CPU_HZ: u32 = 240_000_000;

/// Cycles per microsecond.
pub const CYCLES_PER_MICROSECOND: u32 = CPU_HZ / 1_000_000;

/// The Xtensa cycle counter.
///
/// The timer peripheral is readable but its count does not advance on this
/// board (see `probe`), and a measurement clock that never moves reports every
/// duration as zero. The cycle counter is one instruction and always runs.
#[inline]
pub fn cycles() -> u32 {
    let value: usize;
    unsafe {
        core::arch::asm!("rsr.ccount a2", out("a2") value, options(nomem, nostack));
    }
    value as u32
}

/// Microseconds elapsed between two cycle counts, with one wrap corrected.
#[inline]
pub fn cycles_to_micros(start: u32, end: u32) -> u32 {
    end.wrapping_sub(start) / CYCLES_PER_MICROSECOND
}

/// Microseconds since boot.
///
/// The counter is 32 bits at 1 MHz, so it wraps after ~71 minutes; each
/// observed decrease of the raw count is one wrap.
#[inline]
pub fn raw_ticks() -> u64 {
    let count = capture_counter();
    unsafe {
        if count < LAST_COUNT {
            WRAP_BASE = WRAP_BASE.wrapping_add(1 << 32);
        }
        LAST_COUNT = count;
        WRAP_BASE + u64::from(count)
    }
}

/// Latch the live counter into LO/HI and return it.
#[inline]
fn capture_counter() -> u32 {
    super::platform::write_register(TIMER0_BASE + TIMER_UPDATE, 1);
    while super::platform::read_register(TIMER0_BASE + TIMER_UPDATE) != 0 {
        core::hint::spin_loop();
    }
    read_register(TIMER0_BASE + TIMER_COUNT)
}

pub fn uptime_micros() -> u64 {
    raw_ticks() / TICKS_PER_MICROSECOND
}

pub fn uptime_millis() -> u64 {
    uptime_micros() / 1_000
}

/// What the timer peripheral reports when written to and read back.
///
/// A peripheral whose bus clock is gated reads as a constant, and a constant
/// is exactly what a dead microsecond clock looks like from the outside. This
/// distinguishes "the gate never opened" from "the counter is not running" by
/// asking the registers themselves.
#[derive(Clone, Copy, Debug, Default)]
pub struct TimeProbe {
    /// Value read back from `T0LOADLO` after writing a pattern.
    pub load_lo: u32,
    /// Value read back from `T0ALARMLO` after writing a pattern.
    pub alarm_lo: u32,
    /// The live `T0CONFIG` value.
    pub config: u32,
    /// First and second reads of the counter, with work in between.
    pub count_a: u32,
    pub count_b: u32,
    /// `DPORT_PERIP_CLK_EN_REG` after enabling the timer group clock.
    pub perip_clk_en: u32,
}

impl TimeProbe {
    /// Whether the registers accepted what was written to them.
    pub fn registers_respond(&self) -> bool {
        self.load_lo == 0x0000_1234 && self.alarm_lo == 0x0000_5678
    }

    /// Whether the counter moved between the two reads.
    pub fn counter_advances(&self) -> bool {
        self.count_a != self.count_b
    }
}

/// Ask the timer registers what they think they are.
pub fn probe() -> TimeProbe {
    use super::platform::{read_register, write_register};

    let mut sample = TimeProbe::default();
    // Writing LOADLO alone does not load the counter; only T0LOAD does.
    write_register(TIMER0_BASE + TIMER_LOAD_VALUE, 0x0000_1234);
    write_register(TIMER0_BASE + TIMER_ALARM_LOW, 0x0000_5678);
    sample.load_lo = read_register(TIMER0_BASE + TIMER_LOAD_VALUE);
    sample.alarm_lo = read_register(TIMER0_BASE + TIMER_ALARM_LOW);
    sample.config = read_register(TIMER0_BASE + TIMER_CONTROL);
    // Capture the way the timer requires, not with a bare register read, and
    // wait for the count to change by reading it: a loop of nothing but
    // `spin_loop` hints has no side effects and the optimiser is entitled to
    // delete it, which is why this probe once reported a stuck counter on a
    // timer that was running.
    sample.count_a = capture_counter();
    let mut spins = 0u32;
    loop {
        let observed = capture_counter();
        if observed != sample.count_a || spins >= 2_000_000 {
            break;
        }
        spins = spins.saturating_add(1);
    }
    sample.count_b = capture_counter();
    sample.perip_clk_en = read_register(DPORT_PERIP_CLK_EN);
    sample
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

/// The preemption entry point. Not installed yet: see the module note on why
/// the timer interrupt stays disabled.
#[unsafe(no_mangle)]
pub extern "C" fn timer0_interrupt() {
    crate::arch::xtensa::esp32::scheduler::request_yield();
    super::platform::write_register(TIMER0_BASE + TIMER_INT_CLEAR, 1);
}

#[cfg(test)]
mod tests {
    #[test]
    fn one_tick_is_one_microsecond_by_construction() {
        // The prescale divides the 80 MHz APB clock down to the tick rate.
        assert_eq!(super::TIMER_PRESCALE, 80);
        assert_eq!(80_000_000 / super::TIMER_PRESCALE, 1_000_000);
        assert_eq!(super::TICKS_PER_MICROSECOND, 1);
    }
}
