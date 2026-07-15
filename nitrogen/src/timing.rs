//! Short hardware delays based on the invariant TSC.

use core::sync::atomic::{AtomicU64, Ordering};

static TICKS_PER_US: AtomicU64 = AtomicU64::new(0);

pub fn ticks_per_us() -> u64 {
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

/// Poll `condition_fn` in a spin-loop until it returns `Ok(value)` or the
/// deadline (TSC) expires.
///
/// Each iteration calls `core::hint::spin_loop()` to avoid starving
/// hyper-threads.
///
/// # Arguments
///
/// * `timeout_us` - Microsecond deadline.  `0` polls once (avoids an
///   infinite spin-loop that would hang the kernel).
/// * `condition_fn` - Closure that returns `Some(value)` when the condition
///   is satisfied, or `None` to keep polling.
///
/// # Returns
///
/// `Some(value)` on success, `None` on timeout.
pub fn poll_timeout_us<F, T>(timeout_us: u64, mut condition_fn: F) -> Option<T>
where
    F: FnMut() -> Option<T>,
{
    if timeout_us == 0 {
        // A zero timeout is unsupported and would loop forever.
        // Poll once; if the condition is not satisfied, return None immediately.
        return condition_fn();
    }

    let deadline = unsafe { core::arch::x86_64::_rdtsc() }
        .wrapping_add(timeout_us.saturating_mul(ticks_per_us()));
    loop {
        if let Some(v) = condition_fn() {
            return Some(v);
        }
        if unsafe { core::arch::x86_64::_rdtsc() } >= deadline {
            return None;
        }
        core::hint::spin_loop();
    }
}

/// Poll `condition_fn` until it returns `Ok(value)` or the deadline (TSC)
/// expires, then return `Ok(value)` on success or `Err(())` on timeout.
///
/// Convenience wrapper around [`poll_timeout_us`] for the common pattern
/// where a boolean condition is polled.
pub fn wait_timeout_us<F>(timeout_us: u64, mut condition_fn: F) -> Result<(), ()>
where
    F: FnMut() -> bool,
{
    poll_timeout_us(timeout_us, || condition_fn().then_some(()))
        .ok_or(())
}
