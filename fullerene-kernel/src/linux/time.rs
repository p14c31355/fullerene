// Linux time syscall implementations
use super::runtime::{LinuxRuntime, Runtime, copy_from_user, copy_val_to_user, errno_code};
use super::numbers::*;
use super::types::*;

pub fn sys_nanosleep(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let req = args[0];
    let _rem = args[1];

    if req == 0 {
        return errno_code(EFAULT);
    }

    let ts: LinuxTimespec = LinuxTimespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { copy_val_to_user(req, &ts) }.ok();

    // Read the timespec from user space
    let ts_data = match unsafe { copy_from_user(req, core::mem::size_of::<LinuxTimespec>()) } {
        Ok(d) => d,
        Err(e) => return errno_code(e),
    };
    let ts = unsafe { core::ptr::read_unaligned(ts_data.as_ptr() as *const LinuxTimespec) };
    let sec = ts.tv_sec;
    let nsec = ts.tv_nsec;

    if nsec < 0 || nsec > 999999999 {
        return errno_code(EINVAL);
    }

    // Busy-wait for the requested duration (simplified)
    let total_ns = (sec as u64) * 1_000_000_000 + (nsec as u64);
    if total_ns > 0 {
        // Use a simple delay loop (rdtsc based)
        let start = unsafe { core::arch::x86_64::_rdtsc() };
        let tsc_per_ns = 2; // Approximate: 2GHz CPU → 2 ticks/ns
        let target_ticks = total_ns * tsc_per_ns;
        loop {
            let now = unsafe { core::arch::x86_64::_rdtsc() };
            if now.wrapping_sub(start) >= target_ticks {
                break;
            }
            // Yield periodically to avoid starving other tasks
            if now.wrapping_sub(start) % (1_000_000 * tsc_per_ns) < 100 {
                crate::process::yield_current();
            }
        }
    }

    0
}

pub fn sys_clock_gettime(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _clk_id = args[0] as i32;
    let tp = args[1];

    if tp == 0 {
        return errno_code(EFAULT);
    }

    // Return current time (seconds and nanoseconds)
    let ticks = core::sync::atomic::AtomicU64::load(
        &solvent::GLOBAL_TICK,
        core::sync::atomic::Ordering::Relaxed,
    );

    // Rough estimate: assume timer ticks at ~1000Hz
    let sec = (ticks / 1000) as i64;
    let nsec = ((ticks % 1000) * 1_000_000) as i64;

    let ts = LinuxTimespec { tv_sec: sec, tv_nsec: nsec };
    unsafe { copy_val_to_user(tp, &ts) }.ok();
    0
}

pub fn sys_gettimeofday(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let tv = args[0];
    let _tz = args[1];

    if tv == 0 {
        return 0;
    }

    let ticks = core::sync::atomic::AtomicU64::load(
        &solvent::GLOBAL_TICK,
        core::sync::atomic::Ordering::Relaxed,
    );

    let sec = (ticks / 1000) as i64;
    let usec = ((ticks % 1000) * 1000) as i64;

    let timeval = LinuxTimeval { tv_sec: sec, tv_usec: usec };
    unsafe { copy_val_to_user(tv, &timeval) }.ok();
    0
}

pub fn sys_time(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let t = args[0];
    let ticks = core::sync::atomic::AtomicU64::load(
        &solvent::GLOBAL_TICK,
        core::sync::atomic::Ordering::Relaxed,
    );
    let sec = (ticks / 1000) as i64;
    if t != 0 {
        unsafe { core::ptr::write_volatile(t as *mut i64, sec) };
    }
    if sec < 0 { 0u64 } else { sec as u64 }
}
