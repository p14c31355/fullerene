use core::arch::asm;
use core::sync::atomic::{AtomicBool, Ordering};

pub const TIMER_PPI: u32 = 30;

static IRQ_READY: AtomicBool = AtomicBool::new(false);

pub fn init() -> bool {
    // CNTFRQ_EL0 is firmware-owned on this path. Do not accept an unset
    // counter frequency because all delay and deadline calculations depend
    // on it.
    frequency() != 0
}

/// Some platform boot paths issue syscalls before the GIC is enabled. Syscalls
/// issued during that window cannot rely on a timer interrupt to wake a
/// blocked task, so the sleep boundary uses a bounded counter fallback until
/// this flag is published by the interrupt setup.
pub fn mark_irq_ready() {
    IRQ_READY.store(true, Ordering::Release);
}

pub fn irq_ready() -> bool {
    IRQ_READY.load(Ordering::Acquire)
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

pub fn frequency() -> u64 {
    let value: u64;
    unsafe { asm!("mrs {value}, CNTFRQ_EL0", value = out(reg) value, options(nomem, nostack)) };
    value
}

/// Return the architectural counter in the native ABI's microsecond unit.
///
/// `CNTPCT_EL0` is an implementation-defined tick count, while the shared
/// syscall ABI exposes elapsed time in microseconds. Keep the conversion in
/// one place so UPTIME, CLOCK_GETTIME, and SLEEP cannot drift apart.
pub fn uptime_us() -> u64 {
    let frequency = frequency();
    if frequency == 0 {
        return 0;
    }
    ((counter() as u128 * 1_000_000) / frequency as u128).min(u64::MAX as u128) as u64
}

pub fn delay_ms(milliseconds: u64) {
    delay_ticks(ticks_for_duration(frequency(), milliseconds, 1_000));
}

pub fn delay_us(microseconds: u64) {
    delay_ticks(ticks_for_duration(frequency(), microseconds, 1_000_000));
}

/// Spin until the counter has advanced `ticks`.
///
/// The guard used to be a plain spin budget (2^26 iterations), which a
/// counter read plus `spin_loop` exhausts in well under a second on this
/// core: every long delay - the 45 s panel hold included - returned
/// almost immediately, which is why a hold never looked like one. Watch
/// the counter itself instead, so a real delay always completes and a
/// dead counter still cannot park the boot.
fn delay_ticks(ticks: u64) {
    let start = counter();
    let mut last = start;
    loop {
        if counter().wrapping_sub(start) >= ticks {
            return;
        }
        core::hint::spin_loop();
        if counter() == last {
            // Sampled the same value twice across a spin: if this persists
            // the counter is stopped, and waiting longer is pointless.
            let mut still = 0u32;
            while counter() == last {
                core::hint::spin_loop();
                still += 1;
                if still > 4096 {
                    return;
                }
            }
        }
        last = counter();
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
