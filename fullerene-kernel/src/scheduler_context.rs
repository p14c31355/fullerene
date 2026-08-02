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
use core::alloc::Layout;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use heapless::Vec as HeaplessVec;
use petroleum::common::logging::SystemError;
use x86_64::VirtAddr;
use x86_64::registers::control::Cr3;

use crate::context_switch::switch_context;
use crate::process::{MAX_PROCESSES, Process, ProcessContext, ProcessId, ProcessState};
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
        procs
            .push((pid, process))
            .map_err(|_| SystemError::TooManyProcesses)
    }

    /// Run a closure on a process identified by PID.
    pub fn with_process<F, R>(&self, pid: ProcessId, f: F) -> Option<R>
    where
        F: FnOnce(&mut Process) -> R,
    {
        let mut procs = self.processes.lock();
        procs
            .iter_mut()
            .find(|(id, _)| *id == pid)
            .map(|(_, p)| f(p))
    }

    /// Snapshot a process state for scheduler handoff diagnostics.
    pub fn process_state(&self, pid: ProcessId) -> Option<ProcessState> {
        self.with_process(pid, |process| process.state)
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

    /// Remove terminated processes and reclaim their process-owned memory.
    pub fn cleanup(&self) {
        let current = self.current_pid();
        let mut waiters = HeaplessVec::<ProcessId, MAX_PROCESSES>::new();
        let mut parents = HeaplessVec::<ProcessId, MAX_PROCESSES>::new();
        {
            let mut procs = self.processes.lock();
            let blocked_parents: HeaplessVec<ProcessId, MAX_PROCESSES> = procs
                .iter()
                .filter_map(|(id, process)| (process.state == ProcessState::Blocked).then_some(*id))
                .collect();
            for (id, process) in procs.iter_mut() {
                if !matches!(process.state, ProcessState::Terminated) || id.0 as usize == current {
                    continue;
                }

                if let Some(parent_id) = process.parent_id
                    && blocked_parents.contains(&parent_id)
                {
                    let _ = parents.push(parent_id);
                }
                for waiter in process.resources.cleanup() {
                    let _ = waiters.push(waiter);
                }

                if let Some(kernel_stack_base) = process
                    .kernel_stack
                    .as_u64()
                    .checked_sub(crate::heap::KERNEL_STACK_SIZE as u64)
                    .filter(|&base| base != 0)
                {
                    let layout = Layout::from_size_align(crate::heap::KERNEL_STACK_SIZE, 16)
                        .expect("kernel stack layout");
                    unsafe {
                        petroleum::common::memory::deallocate_layout(
                            kernel_stack_base as *mut u8,
                            layout,
                        );
                    }
                    process.kernel_stack = VirtAddr::new(0);
                }
                if let Some(page_table) = process.page_table.take() {
                    if let Some(pml4_frame) = page_table.pml4_frame() {
                        drop(page_table);
                        crate::memory_management::deallocate_process_page_table(pml4_frame);
                    }
                }
            }
            procs.retain(|(id, p)| {
                !matches!(p.state, ProcessState::Terminated) || id.0 as usize == current
            });
            // Removal can shift every following list index. Re-anchor the
            // round-robin cursor to the process that owns the live CPU state.
            let current_index = procs
                .iter()
                .position(|(id, _)| id.0 as usize == current)
                .unwrap_or(0);
            self.set_schedule_index(current_index);
        }
        for waiter in waiters {
            self.unblock_process(waiter);
        }
        for parent in parents {
            self.unblock_process(parent);
        }
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
        #[cfg(linux_musl_smoke)]
        petroleum::serial::serial_log(format_args!(
            "[linux-smoke] scheduler lock currently held={}\n",
            self.processes.is_locked()
        ));
        let (old_pid, new_pid) = self.with_list(|list| {
            #[cfg(linux_musl_smoke)]
            petroleum::serial::serial_log(format_args!(
                "[linux-smoke] scheduler acquired list len={} index={}\n",
                list.len(),
                self.schedule_index()
            ));
            #[cfg(linux_musl_smoke)]
            for (pid, process) in list.iter() {
                petroleum::serial::serial_log(format_args!(
                    "[linux-smoke] pid={} entry_rip={:#x} entry_rsp={:#x} kernel_rsp={:#x} user={}\n",
                    pid.0,
                    process.context.rip,
                    process.context.registers.rsp,
                    process.context.kernel_rsp,
                    process.context.is_user
                ));
            }
            if list.is_empty() {
                petroleum::scheduler_log!("No processes in list");
                return (None, ProcessId(0));
            }

            // Clamp the schedule index to the valid range in case the process list has shrunk.
            let current_idx = self.schedule_index().min(list.len().saturating_sub(1));
            let mut next_idx = current_idx;
            if list[current_idx].1.state == ProcessState::Terminated {
                // A fault/exit recovery path must never re-enter the task it
                // just marked dead. Prefer a ready task and otherwise use the
                // scheduler's idle process.
                let Some(candidate) = list
                    .iter()
                    .position(|(_, process)| process.state == ProcessState::Ready)
                    .or_else(|| list.iter().position(|(_, process)| process.name == "idle"))
                else {
                    return (Some(list[current_idx].0), ProcessId(0));
                };
                next_idx = candidate;
            } else {
                let start_idx = current_idx;
                // Round‑robin scan.
                loop {
                    next_idx = (next_idx + 1) % list.len();
                    #[cfg(linux_musl_smoke)]
                    petroleum::serial::serial_log(format_args!(
                        "[linux-smoke] scan idx={} pid={} state={:?}\n",
                        next_idx, list[next_idx].0.0, list[next_idx].1.state
                    ));
                    if list[next_idx].1.state == ProcessState::Ready {
                        break;
                    }
                    if next_idx == start_idx {
                        // All blocked → fall back to idle.
                        if let Some(idle) = list.iter().position(|(_, p)| p.name == "idle") {
                            next_idx = idle;
                        }
                        break;
                    }
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

            #[cfg(linux_musl_smoke)]
            petroleum::serial::serial_log(format_args!(
                "[linux-smoke] scheduler selected old={:?} new={}\n",
                old.map(|pid| pid.0),
                new.0
            ));
            (old, new)
        });

        #[cfg(linux_musl_smoke)]
        petroleum::serial::serial_log(format_args!("[linux-smoke] scheduler released list\n"));
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

    /// Cooperatively switch directly to a specific ready process.
    ///
    /// Launchers use this instead of a generic round-robin yield so the
    /// process they just created is guaranteed to receive the next timeslice,
    /// even when unrelated ready tasks already exist.
    pub fn yield_to(&self, new_pid: ProcessId) -> bool {
        let old_pid = ProcessId(self.current_pid() as u64);
        if old_pid.0 == 0 {
            return false;
        }
        if old_pid == new_pid {
            return true;
        }

        let selected = self.with_list(|list| {
            let Some(old_index) = list.iter().position(|(pid, _)| *pid == old_pid) else {
                return false;
            };
            let Some(new_index) = list.iter().position(|(pid, _)| *pid == new_pid) else {
                return false;
            };
            if list[new_index].1.state != ProcessState::Ready {
                return false;
            }

            if list[old_index].1.state == ProcessState::Running {
                list[old_index].1.state = ProcessState::Ready;
            }
            list[new_index].1.state = ProcessState::Running;
            self.set_schedule_index(new_index);
            self.set_current_pid(new_pid.0 as usize);
            true
        });

        if selected {
            unsafe {
                self.context_switch(Some(old_pid), new_pid);
            }
        }
        selected
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
    ///   * Timer and device interrupt handlers do not touch the process list,
    ///     so they cannot race with the pointer window.
    ///   * Cooperative scheduling means no preemption can occur between the
    ///     lock drop and `switch_context`.
    ///
    /// For future SMP support the `ProcessContext` must be ref‑counted
    /// (e.g. `Arc<Mutex<ProcessContext>>`) so that the data stays alive
    /// even when the owning `Process` is dropped.
    pub unsafe fn context_switch(&self, old_pid: Option<ProcessId>, new_pid: ProcessId) {
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
            .filter(|address| address.as_u64() != 0)
            .or_else(|| {
                let kernel = crate::memory_management::kernel_page_table_phys();
                (kernel.as_u64() != 0).then_some(kernel)
            })
            .unwrap_or_else(|| Cr3::read().0.start_address());
        let new_kernel_stack = list
            .iter()
            .find(|(id, _)| *id == new_pid)
            .map(|(_, process)| process.kernel_stack)
            .filter(|stack| stack.as_u64() != 0);
        let old_ctx = old_pid
            .and_then(|pid| list.iter_mut().find(|(id, _)| *id == pid))
            .map(|(_, p)| &mut *p.context as *mut ProcessContext);

        drop(guard);

        if let Some(new) = new_ctx {
            let new_context = unsafe { &*new };
            let plan = crate::context_switch::ContextSwitchPlan::new(
                old_ctx.unwrap_or(core::ptr::null_mut()),
                new_context,
                pt.as_u64(),
                new_kernel_stack.map_or(0, |stack| stack.as_u64()),
            );
            if plan.entry() == crate::context_switch::SwitchEntry::FirstUser {
                crate::klog_fmt!(
                    "[CTX-DIAG] plan user entry rip={:#x} rsp={:#x} cs={:#x} rflags={:#x} ss={:#x} image={:#x}\n",
                    new_context.rip,
                    new_context.registers.rsp,
                    new_context.segments.cs,
                    new_context.rflags,
                    new_context.segments.ss,
                    plan.entry_stack()
                );
                solvent::mark_klog_live_dirty();
                solvent::flush_frame_no_fb();
            }
            unsafe { crate::context_switch::prepare_entry_image(&plan) };
            crate::process::mark_linux_stage(new_pid, "context-switch-prep");
            if let Some(kernel_stack) = new_kernel_stack {
                crate::interrupts::syscall::set_process_kernel_stack(kernel_stack);
            }
            if plan.entry() == crate::context_switch::SwitchEntry::FirstUser {
                crate::interrupts::syscall::prepare_user_entry();
                crate::process::mark_linux_stage(new_pid, "user-entry-prepared");
            } else if plan.entry() == crate::context_switch::SwitchEntry::KernelContinuation {
                crate::interrupts::syscall::prepare_kernel_continuation();
            }
            crate::process::mark_linux_stage(new_pid, "context-switch-enter");
            unsafe { switch_context(&plan) };
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
