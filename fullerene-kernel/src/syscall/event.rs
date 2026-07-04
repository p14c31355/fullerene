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
    let signaled = with_handle_mut(h, |obj| {
        let event = map_handle!(obj, Event, e);
        let mut inner = event.inner.lock();
        if inner.signaled {
            if !inner.manual_reset {
                inner.signaled = false;
            }
            Ok(true)
        } else {
            if timeout_us == 0 {
                return Ok(false);
            }
            let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
            inner.waiters.push(pid);
            Ok(false)
        }
    })?;

    if signaled {
        Ok(0)
    } else if timeout_us == 0 {
        Err(SyscallError::WouldBlock)
    } else {
        crate::process::block_current();
        with_handle_mut(h, |obj| {
            let event = map_handle!(obj, Event, e);
            let mut inner = event.inner.lock();
            if inner.signaled {
                if !inner.manual_reset {
                    inner.signaled = false;
                }
                Ok(0)
            } else {
                Err(SyscallError::TimedOut)
            }
        })
    }
}

pub(crate) fn syscall_signal_event(handle: u64) -> SyscallResult {
    let h = Handle::from_raw(handle);
    check_handle_permission(h, HandlePerms::SIGNAL)?;
    let pids_to_unblock: Vec<process::ProcessId> = with_handle_mut(h, |obj| {
        let event = map_handle!(obj, Event, e);
        let mut inner = event.inner.lock();
        inner.signaled = true;
        let waiters = core::mem::take(&mut inner.waiters);
        Ok(waiters)
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

pub(crate) fn syscall_subscribe_event(_event_type: u64, _callback_info: u64) -> SyscallResult {
    Err(SyscallError::NotSupported)
}
