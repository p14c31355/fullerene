use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use crate::map_handle;
use resonance::Event as ResonanceEvent;

use super::interface::{SyscallError, SyscallResult};
use super::process::{alloc_handle, check_handle_permission, with_handle_mut};
use super::types::*;
use crate::contexts::kernel;
use crate::process;

const EVENT_MANUAL_RESET: u64 = 1;
const MAX_SUBSCRIPTIONS: usize = 64;

pub(crate) fn syscall_create_event(flags: u64) -> SyscallResult {
    let manual_reset = (flags & EVENT_MANUAL_RESET) != 0;
    let inner = Arc::new(Mutex::new(EventInner {
        signaled: false,
        manual_reset,
        waiters: Vec::new(),
    }));
    let handle = alloc_handle(KernelObject::Event(EventState { inner }))?;

    kernel::with_kernel_mut(|k| {
        k.event.push_system(ResonanceEvent::System(
            resonance::event::SystemEvent::Resume,
        ));
    });

    Ok(handle)
}

pub(crate) fn syscall_wait_event(handle: u64, timeout_us: u64) -> SyscallResult {
    let h = Handle::from_raw(handle);
    check_handle_permission(h, HandlePerms::READ)?;

    // Fast path: check if already signaled without blocking
    let already_signaled = with_handle_mut(h, |obj| {
        let event = map_handle!(obj, Event, e);
        let mut inner = event.inner.lock();
        if inner.signaled {
            if !inner.manual_reset {
                inner.signaled = false;
            }
            Ok(true)
        } else {
            Ok(false)
        }
    })?;

    if already_signaled {
        return Ok(0);
    }

    // Non-blocking case
    if timeout_us == 0 {
        return Err(SyscallError::WouldBlock);
    }

    // Enqueue waiter (must drop handle_table lock before block_current)
    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    let should_block = with_handle_mut(h, |obj| {
        let event = map_handle!(obj, Event, e);
        let mut inner = event.inner.lock();
        // Recheck signaled state before blocking to avoid lost wakeup
        if inner.signaled {
            if !inner.manual_reset {
                inner.signaled = false;
            }
            return Ok(false);
        }
        inner.waiters.push(pid);
        Ok(true)
    })?;

    if should_block {
        crate::process::block_current();
    }

    // After waking, check final state
    with_handle_mut(h, |obj| {
        let event = map_handle!(obj, Event, e);
        let mut inner = event.inner.lock();
        if inner.signaled {
            if !inner.manual_reset {
                inner.signaled = false;
            }
            Ok(0)
        } else {
            // Woke up but not signaled - check if it was a timeout or spurious wake
            let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
            if !inner.waiters.contains(&pid) {
                // PID was consumed by signaler - treat as success
                Ok(0)
            } else {
                // Still in waiters list - this is a timeout or spurious wake
                // Remove ourselves from waiters
                inner.waiters.retain(|&p| p != pid);
                Err(SyscallError::TimedOut)
            }
        }
    })
}

pub(crate) fn syscall_signal_event(handle: u64) -> SyscallResult {
    let h = Handle::from_raw(handle);
    check_handle_permission(h, HandlePerms::SIGNAL)?;
    let (pids_to_unblock, _is_manual_reset): (Vec<process::ProcessId>, bool) =
        with_handle_mut(h, |obj| {
            let event = map_handle!(obj, Event, e);
            let mut inner = event.inner.lock();
            inner.signaled = true;
            let is_manual = inner.manual_reset;
            let waiters = if is_manual {
                // Manual reset: wake all waiters
                core::mem::take(&mut inner.waiters)
            } else {
                // Auto reset: wake only one waiter
                inner.waiters.pop().into_iter().collect()
            };
            Ok((waiters, is_manual))
        })?;

    for pid in pids_to_unblock {
        crate::process::unblock_process(pid);
    }

    kernel::with_kernel_mut(|k| {
        k.event.push_system(ResonanceEvent::System(
            resonance::event::SystemEvent::Resume,
        ));
    });

    Ok(0)
}

pub(crate) fn syscall_subscribe_event(event_type: u64, event_handle: u64) -> SyscallResult {
    let h = Handle::from_raw(event_handle);
    // Validate the handle exists and is an event
    let _ = with_handle_mut(h, |obj| {
        let _event = map_handle!(obj, Event, _e);
        Ok(())
    })?;

    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    process::SCHEDULER
        .with_process(pid, |p| {
            let mut subscriptions = p.resources.subscriptions.lock();
            let ht = p.resources.handle_table.lock();

            // Proactively clean up stale subscriptions whose handles have been closed
            subscriptions.retain(|&(_, h_raw)| ht.get(Handle::from_raw(h_raw)).is_some());
            drop(ht);

            // Check if this subscription already exists (idempotent)
            if subscriptions
                .iter()
                .any(|&(t, h)| t == event_type && h == event_handle)
            {
                return Ok(());
            }

            // Enforce capacity limit
            if subscriptions.len() >= MAX_SUBSCRIPTIONS {
                return Err(SyscallError::OutOfMemory);
            }

            subscriptions.push((event_type, event_handle));
            Ok(())
        })
        .ok_or(SyscallError::NoSuchProcess)??;

    Ok(0)
}
