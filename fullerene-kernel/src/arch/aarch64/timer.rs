use core::arch::asm;

pub const TIMER_PPI: u32 = 30;

pub fn init() -> bool {
    // CNTFRQ_EL0 is firmware-owned on this path. Do not accept an unset
    // counter frequency because all delay and deadline calculations depend
    // on it.
    frequency() != 0
}

pub fn arm_ms(milliseconds: u64) {
    let ticks = ticks_for_duration(frequency(), milliseconds, 1_000);
    unsafe {
        asm!("msr CNTP_TVAL_EL0, {ticks}", ticks = in(reg) ticks, options(nostack));
        asm!("msr CNTP_CTL_EL0, {value}", value = in(reg) 1u64, options(nostack));
        asm!("isb", options(nostack));
    }
}

pub fn counter() -> u64 {
    let value: u64;
    unsafe { asm!("mrs {value}, CNTPCT_EL0", value = out(reg) value, options(nomem, nostack)) };
    value
}

fn frequency() -> u64 {
    let value: u64;
    unsafe { asm!("mrs {value}, CNTFRQ_EL0", value = out(reg) value, options(nomem, nostack)) };
    value
}

pub fn delay_ms(milliseconds: u64) {
    let ticks = ticks_for_duration(frequency(), milliseconds, 1_000);
    let start = counter();
    while counter().wrapping_sub(start) < ticks {
        core::hint::spin_loop();
    }
}

pub fn delay_us(microseconds: u64) {
    let ticks = ticks_for_duration(frequency(), microseconds, 1_000_000);
    let start = counter();
    while counter().wrapping_sub(start) < ticks {
        core::hint::spin_loop();
    }
}

fn ticks_for_duration(frequency: u64, units: u64, units_per_second: u64) -> u64 {
    if frequency == 0 || units == 0 {
        return 0;
    }
    let ticks = (frequency as u128 * units as u128).saturating_add((units_per_second - 1) as u128)
        / units_per_second as u128;
    ticks.min(u64::MAX as u128) as u64
}
