//! SchedulerContext — single structure holding all scheduler & process state.
//!
//! # Lock hierarchy
//!
//! ```text
//! SchedulerContext (SCHEDULER)   — lock(process list)
//!     ↑ independent              — no lock taken inside scheduler tick
//! KERNEL (KernelContext)         — lock(subsystems: VFS, window, …)
//!     ↑ called from scheduler    — runtime_tick → with_kernel
//! solvent runtime                — lock(internal)
//! ```
//!
//! `SchedulerContext` lives in its **own static** (not inside `KERNEL`) so
//! the scheduler loop never has to hold two locks at once.  The only lock
//! it takes directly is the per‑tick `processes` lock (brief, for VDSO
//! metadata updates).  Everything else (rendering, shell launch) goes
//! through `KERNEL` or `solvent` which are independent.
//!
//! # NMI recovery
//!
//! The recovery RSP/RIP live in this context so the watchdog has a single
//! place to find the restart target, rather than two orphaned `AtomicU64`
//! statics.

use alloc::boxed::Box;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use heapless::Vec as HeaplessVec;
use petroleum::common::logging::SystemError;
use x86_64::VirtAddr;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::PhysFrame;

use crate::context_switch::switch_context;
use crate::process::{Process, ProcessContext, ProcessId, ProcessState, MAX_PROCESSES};
use crate::vdso;

/// Scheduler tick interval in nanoseconds (for future use).
const _TICK_NANOS: u64 = 2_250_000; // ~2.25 ms ≈ 1 PIT tick

/// ── Global singleton ──────────────────────────────────────────────

pub static SCHEDULER: SchedulerContext = SchedulerContext::new();

/// ── SchedulerContext ──────────────────────────────────────────────

pub struct SchedulerContext {
    // ── Process list (locked) ───────────────────────────────
    processes: spin::Mutex<HeaplessVec<(ProcessId, Box<Process>), MAX_PROCESSES>>,

    // ── Schedule state (lock‑free atomics) ──────────────────
    next_pid: AtomicUsize,
    schedule_index: AtomicUsize,
    current_pid: AtomicUsize,

    // ── Scheduler loop state ────────────────────────────────
    tsc_per_ms: AtomicU64,
    tick_counter: AtomicU64,

    // ── NMI recovery target ─────────────────────────────────
    recovery_rsp: AtomicU64,
    recovery_rip: AtomicU64,
}

impl SchedulerContext {
    /// Compile‑time constructor for a static.
    pub const fn new() -> Self {
        Self {
            processes: spin::Mutex::new(HeaplessVec::new()),
            next_pid: AtomicUsize::new(1),
            schedule_index: AtomicUsize::new(0),
            current_pid: AtomicUsize::new(0),
            tsc_per_ms: AtomicU64::new(0),
            tick_counter: AtomicU64::new(0),
            recovery_rsp: AtomicU64::new(0),
            recovery_rip: AtomicU64::new(0),
        }
    }

    // ── Timer / tick ────────────────────────────────────────

    pub fn set_tsc_per_ms(&self, val: u64) {
        self.tsc_per_ms.store(val, Ordering::Relaxed);
    }
    pub fn get_tsc_per_ms(&self) -> u64 {
        self.tsc_per_ms.load(Ordering::Relaxed)
    }

    /// Increment the tick counter and return the old value (before increment).
    pub fn advance_tick(&self) -> u64 {
        self.tick_counter.fetch_add(1, Ordering::Relaxed)
    }
    pub fn current_tick(&self) -> u64 {
        self.tick_counter.load(Ordering::Relaxed)
    }

    // ── PID allocation ──────────────────────────────────────

    pub fn allocate_pid(&self) -> ProcessId {
        ProcessId(self.next_pid.fetch_add(1, Ordering::Relaxed) as u64)
    }

    // ── Process list access ──────────────────────────────────

    /// Add a new process to the list.
    pub fn add(&self, process: Box<Process>) -> Result<(), SystemError> {
        let mut procs = self.processes.lock();
        if procs.len() >= MAX_PROCESSES {
            return Err(SystemError::TooManyProcesses);
        }
        let pid = process.id;
        // Remove stale entry with same PID (should not happen, but be safe).
        if let Some(pos) = procs.iter().position(|(id, _)| *id == pid) {
            let _ = procs.swap_remove(pos);
        }
        procs.push((pid, process)).map_err(|_| SystemError::TooManyProcesses)
    }

    /// Run a closure on a process identified by PID.
    pub fn with_process<F, R>(&self, pid: ProcessId, f: F) -> Option<R>
    where
        F: FnOnce(&mut Process) -> R,
    {
        let mut procs = self.processes.lock();
        procs.iter_mut().find(|(id, _)| *id == pid).map(|(_, p)| f(p))
    }

    /// Run a closure on every process.
    pub fn for_each_process<F>(&self, mut f: F)
    where
        F: FnMut(&Process),
    {
        let procs = self.processes.lock();
        for (_, p) in procs.iter() {
            f(p.as_ref());
        }
    }

    /// Run a mutable closure on every process.
    pub fn for_each_process_mut<F>(&self, mut f: F)
    where
        F: FnMut(&mut Process),
    {
        let mut procs = self.processes.lock();
        for (_, p) in procs.iter_mut() {
            f(p.as_mut());
        }
    }

    /// Run a closure on the entire process list (raw access).
    pub fn with_list<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut HeaplessVec<(ProcessId, Box<Process>), MAX_PROCESSES>) -> R,
    {
        let mut procs = self.processes.lock();
        f(&mut *procs)
    }

    /// Count of all processes.
    pub fn count(&self) -> usize {
        self.processes.lock().len()
    }

    /// Count of ready+running processes.
    pub fn active_count(&self) -> usize {
        self.processes
            .lock()
            .iter()
            .filter(|(_, p)| matches!(p.state, ProcessState::Ready | ProcessState::Running))
            .count()
    }

    /// Remove terminated processes.
    pub fn cleanup(&self) {
        let mut procs = self.processes.lock();
        procs.retain(|(_, p)| !matches!(p.state, ProcessState::Terminated));
    }

    // ── Current PID ─────────────────────────────────────────

    pub fn current_pid(&self) -> usize {
        self.current_pid.load(Ordering::SeqCst)
    }

    pub fn set_current_pid(&self, pid: usize) {
        self.current_pid.store(pid, Ordering::SeqCst);
    }

    pub fn schedule_index(&self) -> usize {
        self.schedule_index.load(Ordering::SeqCst)
    }

    pub fn set_schedule_index(&self, idx: usize) {
        self.schedule_index.store(idx, Ordering::SeqCst);
    }

    // ── Scheduling (round‑robin) ────────────────────────────

    /// Select the next ready process and update global state.
    /// Returns `(old_pid, new_pid)`.
    pub fn schedule_next(&self) -> (Option<ProcessId>, ProcessId) {
        petroleum::scheduler_log!("Starting process scheduling");

        let (old_pid, new_pid) = self.with_list(|list| {
            if list.is_empty() {
                petroleum::scheduler_log!("No processes in list");
                return (None, ProcessId(0));
            }

            // Clamp the schedule index to the valid range in case the process list has shrunk.
            let current_idx = self.schedule_index().min(list.len().saturating_sub(1));
            let start_idx = current_idx;
            let mut next_idx = current_idx;

            // Round‑robin scan
            loop {
                next_idx = (next_idx + 1) % list.len();
                if list[next_idx].1.state == ProcessState::Ready {
                    break;
                }
                if next_idx == start_idx {
                    // All blocked → fall back to idle
                    if let Some(idle) = list.iter().position(|(_, p)| p.name == "idle") {
                        next_idx = idle;
                    }
                    break;
                }
            }

            let old = if current_idx < list.len() {
                let pid = list[current_idx].0;
                Some(pid)
            } else {
                None
            };
            let new = list[next_idx].0;

            self.set_schedule_index(next_idx);
            self.set_current_pid(new.0 as usize);

            if current_idx != next_idx {
                if let Some((_, cur)) = list.get_mut(current_idx) {
                    if cur.state == ProcessState::Running {
                        cur.state = ProcessState::Ready;
                    }
                }
                if let Some((_, nxt)) = list.get_mut(next_idx) {
                    nxt.state = ProcessState::Running;
                }
            }

            (old, new)
        });

        (old_pid, new_pid)
    }

    /// Block the current process and switch to the next.
    pub fn block_current(&self) {
        let pid = ProcessId(self.current_pid.load(Ordering::SeqCst) as u64);
        if pid.0 == 0 {
            return;
        }
        self.with_process(pid, |p| p.state = ProcessState::Blocked);
        let (old, new) = self.schedule_next();
        if let (Some(o), n) = (old, new) {
            if o != n {
                unsafe { self.context_switch(Some(o), n) };
            }
        }
    }

    /// Unblock a process (set it back to Ready).
    pub fn unblock_process(&self, pid: ProcessId) {
        self.with_process(pid, |p| {
            if p.state == ProcessState::Blocked {
                p.state = ProcessState::Ready;
            }
        });
    }

    /// Yield the current process.
    pub fn yield_current(&self) {
        let old_pid_val = self.current_pid();
        if old_pid_val == 0 {
            return;
        }
        let (old, new) = self.schedule_next();
        if let (Some(o), n) = (old, new) {
            if o != n {
                unsafe { self.context_switch(Some(o), n) };
            }
        }
    }

    /// Raw context switch — updates CR3 when needed.
    ///
    /// # Safety
    ///
    /// Raw context pointers are extracted while holding the process-list
    /// spinlock, then dereferenced after the lock is released.  This is safe
    /// **only** because the kernel is currently single-core (UP) and uses
    /// cooperative scheduling:
    ///
    ///   * No other core can concurrently terminate/clean up a process.
    ///   * Interrupt handlers (interrupt‑gate, IF=0) never touch the process
    ///     list, so they cannot race with the pointer window.
    ///   * Cooperative scheduling means no preemption can occur between the
    ///     lock drop and `switch_context`.
    ///
    /// For future SMP support the `ProcessContext` must be ref‑counted
    /// (e.g. `Arc<Mutex<ProcessContext>>`) so that the data stays alive
    /// even when the owning `Process` is dropped.
    pub unsafe fn context_switch(
        &self,
        old_pid: Option<ProcessId>,
        new_pid: ProcessId,
    ) {
        // Same-process no‑op
        if old_pid == Some(new_pid) {
            return;
        }

        let mut guard = self.processes.lock();
        let list = &mut *guard;

        let new_ctx = list
            .iter()
            .find(|(id, _)| *id == new_pid)
            .map(|(_, p)| &*p.context as *const ProcessContext);
        let pt = list
            .iter()
            .find(|(id, _)| *id == new_pid)
            .map(|(_, p)| p.page_table_phys_addr)
            .unwrap_or(x86_64::PhysAddr::new(0));
        let old_ctx = old_pid
            .and_then(|pid| list.iter_mut().find(|(id, _)| *id == pid))
            .map(|(_, p)| &mut *p.context as *mut ProcessContext);
        drop(guard);

        if let Some(new) = new_ctx {
            if pt.as_u64() != 0 {
                let new_frame = PhysFrame::containing_address(pt);
                let (current_frame, _) = Cr3::read();
                if new_frame != current_frame {
                    Cr3::write(new_frame, x86_64::registers::control::Cr3Flags::empty());
                }
            }
            let old_ref = old_ctx.map(|ptr| unsafe { &mut *ptr });
            unsafe { switch_context(old_ref, &*new) };
        }
    }

    /// Unblock parent processes waiting for a child.
    pub fn unblock_waiting_parents(&self, child_pid: ProcessId) {
        let parent_to_unblock = self.with_list(|list| {
            list.iter()
                .find(|(id, _)| *id == child_pid)
                .and_then(|(_, proc)| proc.parent_id)
                .filter(|&parent_id| {
                    list.iter()
                        .find(|(id, _)| *id == parent_id)
                        .map_or(false, |(_, parent)| parent.state == ProcessState::Blocked)
                })
        });
        if let Some(parent_id) = parent_to_unblock {
            self.unblock_process(parent_id);
        }
    }

    // ── NMI recovery ────────────────────────────────────────

    pub fn set_recovery(&self, rsp: VirtAddr, rip: VirtAddr) {
        self.recovery_rsp.store(rsp.as_u64(), Ordering::Release);
        self.recovery_rip.store(rip.as_u64(), Ordering::Release);
    }

    pub fn recovery_target(&self) -> Option<(VirtAddr, VirtAddr)> {
        let rsp = self.recovery_rsp.load(Ordering::Acquire);
        let rip = self.recovery_rip.load(Ordering::Acquire);
        if rsp != 0 && rip != 0 {
            Some((VirtAddr::new(rsp), VirtAddr::new(rip)))
        } else {
            None
        }
    }

    // ── VDSO metadata update ────────────────────────────────

    /// Write uptime / wall‑clock into every process's VDSO page.
    /// Called once per scheduler tick.
    pub fn update_vdso_all(&self, now_us: u64, wall_us: u64) {
        let mut procs = self.processes.lock();
        for (_, proc) in procs.iter_mut() {
            if let Some(ref vdso_ref) = proc.vdso_page {
                vdso::update_vdso_metadata(now_us, wall_us, vdso_ref.kernel_ptr);
            }
        }
    }
}
