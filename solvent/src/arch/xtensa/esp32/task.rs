//! A conservative cooperative task model for the first ESP32 profile.

use alloc::string::String;
use core::sync::atomic::{AtomicBool, Ordering};

static TASK_YIELD: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    Running,
    Finished,
}

pub struct Task {
    pub name: String,
    pub state: TaskState,
    pub entry: usize,
    pub stack_size: usize,
}

impl Task {
    pub const fn ready(name: String, entry: usize, stack_size: usize) -> Self {
        Self {
            name,
            state: TaskState::Ready,
            entry,
            stack_size,
        }
    }
}

pub fn request_yield() {
    TASK_YIELD.store(true, Ordering::Release);
}

pub fn take_yield_request() -> bool {
    TASK_YIELD.swap(false, Ordering::AcqRel)
}
