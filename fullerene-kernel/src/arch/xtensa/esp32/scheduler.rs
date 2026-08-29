//! Single-core deterministic round-robin scheduler.

use crate::arch::xtensa::esp32::{interrupts::TaskContext, memory, time};
use alloc::{string::String, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

static YIELD_REQUESTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    Running,
    Finished,
}

pub struct Task {
    pub id: u32,
    pub name: String,
    pub state: TaskState,
    pub entry: usize,
    pub stack: &'static mut [u8],
    pub context: TaskContext,
}

pub struct Scheduler {
    tasks: Vec<Task>,
}

impl Scheduler {
    pub const fn new() -> Self {
        Self { tasks: Vec::new() }
    }

    pub fn spawn(&mut self, name: &str, entry: usize, stack_size: usize) -> Option<&Task> {
        if self.tasks.len() >= 12 {
            return None;
        }
        let stack = memory::allocate_stack(stack_size)?;
        let context = TaskContext::empty();
        self.tasks.push(Task {
            id: crate::arch::xtensa::esp32::runtime::next_task_id(),
            name: String::from(name),
            state: TaskState::Ready,
            entry,
            stack,
            context,
        });
        self.tasks.last()
    }

    pub fn run(&mut self) -> ! {
        // The real Xtensa context switch cannot use SP as an inline-asm
        // operand, and the current switch protocol is not safe until the
        // exception-frame handoff is implemented. Fail loudly rather than
        // silently corrupting task stacks.
        if !self.tasks.is_empty() {
            crate::arch::xtensa::esp32::runtime::panic_message(
                "Xtensa scheduler context switch is not implemented",
            );
        }
        loop {
            YIELD_REQUESTED.store(false, Ordering::Release);
            time::sleep_ticks(1);
        }
    }

    pub fn names(&self) -> Vec<String> {
        self.tasks.iter().map(|task| task.name.clone()).collect()
    }
}

pub fn request_yield() {
    YIELD_REQUESTED.store(true, Ordering::Release);
}

pub fn scheduler_yield() {
    request_yield();
}
