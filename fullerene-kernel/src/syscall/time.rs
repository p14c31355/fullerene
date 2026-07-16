use alloc::vec::Vec;

use crate::map_handle;
use core::sync::atomic::{AtomicU64, Ordering};
use petroleum::common::memory::UserSlice;

use super::interface::{SyscallError, SyscallResult};
use super::process::{alloc_handle, with_handle};
use super::types::*;
use crate::process;

static UPTIME_US: AtomicU64 = AtomicU64::new(0);

fn uptime_us() -> u64 {
    UPTIME_US.load(Ordering::Relaxed)
}

pub fn tick_uptime(delta_us: u64) {
    UPTIME_US.fetch_add(delta_us, Ordering::Relaxed);
    check_and_fire_timers();
}

pub fn check_and_fire_timers() {
    let now_ns = uptime_us() * 1000;

    let expired: Vec<(process::ProcessId, Handle)> = {
        let mut expired_timers = Vec::new();
        process::SCHEDULER.with_list(|list| {
            for (owner_pid, proc) in list.iter_mut() {
                let mut ht = proc.resources.handle_table.lock();
                for (_handle, obj) in ht.entries_mut() {
                    if let KernelObject::Timer(timer) = obj {
                        if !timer.fired && now_ns >= timer.deadline_ns {
                            timer.fired = true;
                            expired_timers.push((*owner_pid, timer.event_handle));
                        }
                    }
                }
            }
        });
        expired_timers
    };

    for (owner_pid, event_handle) in expired {
        let waiters_to_unblock: Vec<process::ProcessId> = process::SCHEDULER
            .with_process(owner_pid, |proc| {
                let mut ht = proc.resources.handle_table.lock();
                if let Some(KernelObject::Event(e)) = ht.get_mut(event_handle) {
                    let mut inner = e.inner.lock();
                    inner.signaled = true;
                    core::mem::take(&mut inner.waiters)
                } else {
                    Vec::new()
                }
            })
            .unwrap_or_default();
        for pid in waiters_to_unblock {
            crate::process::unblock_process(pid);
        }
    }
}

pub(crate) fn syscall_clock_gettime(clock_id: u64, timespec_buf: *mut u8) -> SyscallResult {
    if timespec_buf.is_null() {
        return Err(SyscallError::InvalidArgument);
    }
    petroleum::validate_user_buffer(timespec_buf as usize, 16, false)?;

    let (sec, nsec) = match clock_id {
        0 => {
            let us = uptime_us();
            (us / 1_000_000, ((us % 1_000_000) * 1000))
        }
        1 => (0, 0),
        _ => return Err(SyscallError::InvalidArgument),
    };

    let slice =
        UserSlice::new(timespec_buf, 16, true).map_err(|_| SyscallError::InvalidArgument)?;
    let mut kernel_buf = [0u8; 16];
    kernel_buf[0..8].copy_from_slice(&sec.to_ne_bytes());
    kernel_buf[8..16].copy_from_slice(&nsec.to_ne_bytes());
    unsafe { slice.copy_to_user(&kernel_buf) }.map_err(|_| SyscallError::InvalidArgument)?;

    Ok(0)
}

pub(crate) fn syscall_timer_create(
    _clock_id: u64,
    deadline_ns: u64,
    event_handle: u64,
) -> SyscallResult {
    let h = Handle::from_raw(event_handle);

    with_handle(h, |obj| {
        map_handle!(obj, Event, _e);
        Ok(())
    })?;

    let timer = TimerState {
        deadline_ns,
        event_handle: h,
        fired: false,
    };
    alloc_handle(KernelObject::Timer(timer))
}

pub(crate) fn syscall_sleep(us: u64) -> SyscallResult {
    let deadline = uptime_us() + us;

    if us < 1000 {
        let start = uptime_us();
        while uptime_us() < start + us {
            core::hint::spin_loop();
        }
        Ok(0)
    } else {
        while uptime_us() < deadline {
            process::yield_current();
        }
        Ok(0)
    }
}

pub(crate) fn syscall_uptime(buf: *mut u8) -> SyscallResult {
    if buf.is_null() {
        return Err(SyscallError::InvalidArgument);
    }
    petroleum::validate_user_buffer(buf as usize, 8, false)?;
    let us = uptime_us();
    let slice = UserSlice::new(buf, 8, true).map_err(|_| SyscallError::InvalidArgument)?;
    let kernel_buf = us.to_ne_bytes();
    unsafe { slice.copy_to_user(&kernel_buf) }.map_err(|_| SyscallError::InvalidArgument)?;
    Ok(0)
}
