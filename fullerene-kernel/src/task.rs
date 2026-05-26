//! Cooperative tasking & async runtime for Fullerene OS.
//!
//! Provides a minimal async runtime on top of the kernel's preemptive
//! scheduler.  Tasks are spawned as kernel processes; cooperativity is
//! achieved via manual `yield_now()` and `block_on()` primitives.
//!
//! # Architecture
//!
//! ```
//! Task::spawn(future) → kernel process (preemptive)
//!     future.await → poll() → Pending → yield / block
//!     waker.wake() → unblock process → poll() again
//! ```
//!
//! The runtime uses the existing `ProcessManager` for process lifecycle
//! and a simple waker that unblocks the owning process.

use alloc::boxed::Box;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use x86_64::VirtAddr;

use crate::process::{self, ProcessId, ProcessState};

// ── Waker ──────────────────────────────────────────────────────

/// A waker that unblocks a kernel process via its PID.
struct ProcessWaker {
    pid: u64,
    woken: AtomicBool,
}

unsafe fn waker_clone(raw: *const ()) -> RawWaker {
    let w = &*(raw as *const ProcessWaker);
    let boxed = Box::new(ProcessWaker {
        pid: w.pid,
        woken: AtomicBool::new(w.woken.load(Ordering::Relaxed)),
    });
    RawWaker::new(Box::into_raw(boxed) as *const (), &WAKER_VTABLE)
}

unsafe fn waker_wake(raw: *const ()) {
    let w = &*(raw as *const ProcessWaker);
    w.woken.store(true, Ordering::Release);
    process::unblock_process(ProcessId(w.pid));
}

unsafe fn waker_wake_by_ref(raw: *const ()) {
    let w = &*(raw as *const ProcessWaker);
    w.woken.store(true, Ordering::Release);
    process::unblock_process(ProcessId(w.pid));
}

unsafe fn waker_drop(raw: *const ()) {
    drop(Box::from_raw(raw as *mut ProcessWaker));
}

static WAKER_VTABLE: RawWakerVTable =
    RawWakerVTable::new(waker_clone, waker_wake, waker_wake_by_ref, waker_drop);

fn create_waker(pid: u64) -> Waker {
    let pw = Box::new(ProcessWaker {
        pid,
        woken: AtomicBool::new(false),
    });
    let raw = RawWaker::new(Box::into_raw(pw) as *const (), &WAKER_VTABLE);
    unsafe { Waker::from_raw(raw) }
}

// ── Task handle ────────────────────────────────────────────────

/// Opaque handle to a spawned async task.
pub struct TaskHandle {
    pub pid: u64,
}

impl TaskHandle {
    /// Block the current process until the task completes.
    ///
    /// This is a **cooperative** wait: the caller yields to the
    /// scheduler and is woken when the task's waker fires.
    pub fn join(self) {
        loop {
            let terminated = process::PROCESS_MANAGER
                .with_process(ProcessId(self.pid), |p| {
                    matches!(p.state, ProcessState::Terminated)
                });
            if terminated.unwrap_or(true) {
                return;
            }
            process::yield_current();
        }
    }
}

// ── Spawn ──────────────────────────────────────────────────────

/// Spawn an async task as a kernel process.
///
/// The task runs on its own kernel stack and is scheduled preemptively.
/// Cooperative behaviour (yield / block) is driven by the future's poll
/// returning `Poll::Pending`.
pub fn spawn<F>(future: F) -> Result<TaskHandle, petroleum::common::logging::SystemError>
where
    F: Future<Output = ()> + Send + 'static,
{
    // We need a type‑erased entry point.  We use a generic entry function
    // and stash the future pointer via ProcessManager's with_process.
    let pid = process::create_process(
        "async-task",
        VirtAddr::new(task_entry::<F> as *const () as u64),
        false, // kernel process
    )?;

    // Store the future pointer through the process's user_stack field
    // (unused for kernel processes).
    let boxed: Box<dyn Future<Output = ()> + Send> = Box::new(future);
    let raw = Box::into_raw(Box::new(boxed));
    process::PROCESS_MANAGER.with_process(ProcessId(pid.0 as u64), |p| {
        p.user_stack = VirtAddr::new(raw as u64);
    });

    Ok(TaskHandle { pid: pid.0 as u64 })
}

/// Entry point for spawned async tasks.
///
/// Extracts the future pointer, polls it to completion, then terminates.
extern "C" fn task_entry<F: Future<Output = ()> + Send + 'static>() {
    let pid = process::current_pid().expect("task_entry: no current PID");
    let raw = process::PROCESS_MANAGER
        .with_process(pid, |p| {
            p.user_stack.as_u64() as *mut Box<dyn Future<Output = ()> + Send>
        })
        .expect("task_entry: process not found");

    let boxed: Box<Box<dyn Future<Output = ()> + Send>> = unsafe { Box::from_raw(raw) };
    let mut future: Pin<Box<dyn Future<Output = ()> + Send>> = (*boxed).into();

    loop {
        let waker = create_waker(pid.0 as u64);
        let mut cx = Context::from_waker(&waker);
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(()) => break,
            Poll::Pending => {
                process::block_current();
            }
        }
    }

    process::terminate_process(pid, 0);
}

// ── Block-on (synchronous wait) ────────────────────────────────

/// Run a future to completion on the current kernel thread.
///
/// Spins polling the future; yields cooperatively via `cpu_pause()`
/// between polls.  Suitable only for trivial / short‑lived futures
/// in kernel context.
pub fn block_on<F: Future>(mut future: F) -> F::Output {
    let mut future = unsafe { Pin::new_unchecked(&mut future) };
    let pid = process::current_pid()
        .map(|p| p.0 as u64)
        .unwrap_or(0);
    loop {
        let waker = create_waker(pid);
        let mut cx = Context::from_waker(&waker);
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(out) => return out,
            Poll::Pending => {
                petroleum::cpu_pause();
            }
        }
    }
}