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
//!
//! # Task Manager / Monitor
//!
//! The `TaskManager` singleton tracks spawned tasks and exposes a
//! process list (name, PID, state) for the shell `tasks` command
//! and the task-overview overlay.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use spin::Mutex;
use x86_64::VirtAddr;

use crate::process::{self, ProcessId, ProcessState};

// ── Waker ──────────────────────────────────────────────────────

struct ProcessWaker {
    pid: u64,
    woken: AtomicBool,
}

unsafe fn waker_clone(raw: *const ()) -> RawWaker {
    unsafe {
        let w = &*(raw as *const ProcessWaker);
        let boxed = Box::new(ProcessWaker {
            pid: w.pid,
            woken: AtomicBool::new(w.woken.load(Ordering::Relaxed)),
        });
        RawWaker::new(Box::into_raw(boxed) as *const (), &WAKER_VTABLE)
    }
}

unsafe fn waker_wake(raw: *const ()) {
    unsafe {
        let w = Box::from_raw(raw as *mut ProcessWaker);
        w.woken.store(true, Ordering::Release);
        process::unblock_process(ProcessId(w.pid));
    }
}

unsafe fn waker_wake_by_ref(raw: *const ()) {
    unsafe {
        let w = &*(raw as *const ProcessWaker);
        w.woken.store(true, Ordering::Release);
        process::unblock_process(ProcessId(w.pid));
    }
}

unsafe fn waker_drop(raw: *const ()) {
    unsafe {
        drop(Box::from_raw(raw as *mut ProcessWaker));
    }
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
    pub fn join(self) {
        loop {
            let terminated = process::PROCESS_MANAGER.with_process(ProcessId(self.pid), |p| {
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
pub fn spawn<F>(future: F) -> Result<TaskHandle, petroleum::common::logging::SystemError>
where
    F: Future<Output = ()> + Send + 'static,
{
    let boxed: Box<dyn Future<Output = ()> + Send> = Box::new(future);
    let raw = Box::into_raw(Box::new(boxed));

    let pid = x86_64::instructions::interrupts::without_interrupts(
        || -> Result<_, petroleum::common::logging::SystemError> {
            let p = process::create_process(
                "async-task",
                VirtAddr::new(task_entry::<F> as *const () as u64),
                false,
            )?;
            process::PROCESS_MANAGER.with_process(ProcessId(p.0 as u64), |pr| {
                pr.task_data = raw as u64;
                pr.state = ProcessState::Ready;
            });
            Ok(p)
        },
    )?;

    Ok(TaskHandle { pid: pid.0 as u64 })
}

/// Entry point for spawned async tasks.
extern "C" fn task_entry<F: Future<Output = ()> + Send + 'static>() {
    let pid = process::current_pid().expect("task_entry: no current PID");
    let raw = process::PROCESS_MANAGER
        .with_process(pid, |p| {
            p.task_data as *mut Box<dyn Future<Output = ()> + Send>
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

pub fn block_on<F: Future>(mut future: F) -> F::Output {
    let mut future = unsafe { Pin::new_unchecked(&mut future) };
    let pid = process::current_pid().map(|p| p.0 as u64).unwrap_or(0);
    loop {
        let waker = create_waker(pid);
        let mut cx = Context::from_waker(&waker);
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(out) => return out,
            Poll::Pending => {
                process::block_current();
            }
        }
    }
}

// ── Task Manager / Monitor ────────────────────────────────────

/// Lightweight tracked task entry.
#[derive(Debug, Clone)]
pub struct TrackedTask {
    pub pid: u64,
    pub name: &'static str,
    pub state: &'static str,
    pub is_user: bool,
}

/// Global task manager for listing/monitoring running tasks.
pub struct TaskManager {
    entries: Mutex<Vec<TrackedTask>>,
}

impl TaskManager {
    pub const fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }

    /// Register a task in the monitor.
    pub fn register(&self, pid: u64, name: &'static str, is_user: bool) {
        self.entries.lock().push(TrackedTask {
            pid,
            name,
            state: "ready",
            is_user,
        });
    }

    /// Update a task's state.
    pub fn update_state(&self, pid: u64, state: &'static str) {
        if let Some(e) = self.entries.lock().iter_mut().find(|e| e.pid == pid) {
            e.state = state;
        }
    }

    /// Remove a terminated task.
    pub fn remove(&self, pid: u64) {
        self.entries.lock().retain(|e| e.pid != pid);
    }

    /// Get a snapshot of all tracked tasks.
    pub fn snapshot(&self) -> Vec<TrackedTask> {
        self.entries.lock().clone()
    }

    /// Get a formatted task list for the shell.
    pub fn format_task_list(&self) -> alloc::string::String {
        let entries = self.entries.lock();
        let mut out = alloc::string::String::from("PID   NAME             STATE     TYPE\n");
        out.push_str("----  ----------------  --------  ----\n");
        for e in entries.iter() {
            let ttype = if e.is_user { "user" } else { "kern" };
            let line = alloc::format!(
                "{:<4}  {:<16}  {:<8}  {}\n",
                e.pid, e.name, e.state, ttype
            );
            out.push_str(&line);
        }
        out
    }
}

/// Global task manager instance.
pub static TASK_MANAGER: TaskManager = TaskManager::new();

/// Initialize the task manager.  Does a sweep of existing processes
/// from the ProcessManager to seed the initial task list.
pub fn init_task_manager() {
    crate::process::PROCESS_MANAGER.with_list(|list| {
        for (_, proc) in list.iter() {
            TASK_MANAGER.register(proc.id.0, proc.name, proc.is_user);
        }
    });
}