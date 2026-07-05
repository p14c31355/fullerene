//! Short hardware delays based on the invariant TSC.

use core::sync::atomic::{AtomicU64, Ordering};

static TICKS_PER_US: AtomicU64 = AtomicU64::new(0);

fn ticks_per_us() -> u64 {
    let cached = TICKS_PER_US.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    let max_leaf = core::arch::x86_64::__cpuid(0).eax;
    let measured = if max_leaf >= 0x15 {
        let ratio = core::arch::x86_64::__cpuid(0x15);
        (ratio.eax != 0 && ratio.ebx != 0 && ratio.ecx != 0)
            .then(|| u64::from(ratio.ecx) * u64::from(ratio.ebx) / u64::from(ratio.eax) / 1_000_000)
    } else {
        None
    }
    .or_else(|| {
        (max_leaf >= 0x16)
            .then(|| core::arch::x86_64::__cpuid(0x16).eax)
            .and_then(|mhz| (mhz != 0).then_some(u64::from(mhz)))
    })
    .unwrap_or(5_000)
    .max(1);
    let _ = TICKS_PER_US.compare_exchange(0, measured, Ordering::Relaxed, Ordering::Relaxed);
    TICKS_PER_US.load(Ordering::Relaxed)
}

pub fn delay_us(microseconds: u64) {
    let target = microseconds.saturating_mul(ticks_per_us());
    let start = unsafe { core::arch::x86_64::_rdtsc() };
    while unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) < target {
        core::hint::spin_loop();
    }
}

pub fn delay_ms(milliseconds: u64) {
    delay_us(milliseconds.saturating_mul(1_000));
}
