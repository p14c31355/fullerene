use core::arch::asm;

pub const TIMER_PPI: u32 = 30;

pub fn init() {
    // Reading CNTFRQ/CNTPCT is safe before enabling the IRQ path. The
    // platform layer arms the timer after the GIC is configured.
}

pub fn arm_ms(milliseconds: u64) {
    let ticks = frequency().saturating_mul(milliseconds) / 1000;
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
    let ticks = frequency().saturating_mul(milliseconds) / 1000;
    let start = counter();
    while counter().wrapping_sub(start) < ticks {
        core::hint::spin_loop();
    }
}

pub fn delay_us(microseconds: u64) {
    let ticks = frequency().saturating_mul(microseconds) / 1_000_000;
    let start = counter();
    while counter().wrapping_sub(start) < ticks {
        core::hint::spin_loop();
    }
}
