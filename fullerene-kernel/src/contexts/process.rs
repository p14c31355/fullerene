//! ProcessContext — scheduler, task manager, process table.
//!
//! Aggregates the three process-related subsystems that were previously
//! scattered across `process.rs`, `scheduler.rs`, and `task.rs`.

use spin::Mutex;

/// Process scheduling context.
pub struct SchedulerContext {
    /// Whether the scheduler has been initialised.
    pub initialized: bool,
    /// Number of context switches performed.
    pub switch_count: u64,
}

impl SchedulerContext {
    pub const fn new() -> Self {
        Self {
            initialized: false,
            switch_count: 0,
        }
    }
}

/// Task management context (async tasks, futures).
pub struct TaskManagerContext {
    /// Number of active async tasks.
    pub active_tasks: usize,
    /// Whether the task manager has been initialised.
    pub initialized: bool,
}

impl TaskManagerContext {
    pub const fn new() -> Self {
        Self {
            active_tasks: 0,
            initialized: false,
        }
    }
}

/// Process table context (PID registry, process lifecycle).
pub struct ProcessTableContext {
    /// Total processes created since boot.
    pub total_created: u64,
    /// Currently active processes.
    pub active_count: usize,
    /// Whether the process table has been initialised.
    pub initialized: bool,
}

impl ProcessTableContext {
    pub const fn new() -> Self {
        Self {
            total_created: 0,
            active_count: 0,
            initialized: false,
        }
    }
}

/// Aggregated process context.
pub struct ProcessContext {
    pub scheduler: SchedulerContext,
    pub tasks: TaskManagerContext,
    pub process_table: ProcessTableContext,
}

// ProcessContext lives behind a Mutex; interior Send+Sync covered by sub-fields.
unsafe impl Send for ProcessContext {}
unsafe impl Sync for ProcessContext {}

impl ProcessContext {
    pub const fn new() -> Self {
        Self {
            scheduler: SchedulerContext::new(),
            tasks: TaskManagerContext::new(),
            process_table: ProcessTableContext::new(),
        }
    }

    /// True when all process subsystems are initialised.
    pub fn is_ready(&self) -> bool {
        self.scheduler.initialized
            && self.tasks.initialized
            && self.process_table.initialized
    }
}

// ── Global singleton ──────────────────────────────────────────
static PROCESS: Mutex<Option<ProcessContext>> = Mutex::new(None);

pub fn init_process() {
    *PROCESS.lock() = Some(ProcessContext::new());
}

pub fn get_process() -> &'static Mutex<Option<ProcessContext>> {
    &PROCESS
}

pub fn with_process_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut ProcessContext) -> R,
{
    PROCESS.lock().as_mut().map(f)
}

pub fn with_process<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&ProcessContext) -> R,
{
    PROCESS.lock().as_ref().map(f)
}