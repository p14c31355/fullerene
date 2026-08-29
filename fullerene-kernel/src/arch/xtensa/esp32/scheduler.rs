//! Single-core cooperative round-robin scheduler.
//!
//! The embedded profile starts with cooperative tasks so the first context
//! switch is small and inspectable. Timer preemption will add interrupt-frame
//! save/restore on top of this protocol; it must not bypass the stack-owner
//! rules established here.

use crate::arch::xtensa::esp32::{interrupts::TaskContext, memory};
use alloc::{string::String, vec::Vec};
use core::arch::global_asm;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static YIELD_REQUESTED: AtomicBool = AtomicBool::new(false);
static CURRENT_TASK: AtomicUsize = AtomicUsize::new(0);
static mut SCHEDULER_STACK_POINTER: usize = 0;

global_asm!(
    ".section .text.xtensa_switch_to, \"ax\"",
    ".p2align 2",
    ".global xtensa_switch_to",
    "xtensa_switch_to:",
    // The target is built with the windowed register feature disabled. This
    // makes the embedded scheduler ABI call0-based and keeps a task switch to
    // SP, return address, SAR, and the four ABI callee-saved registers.
    "addi a1, a1, -32",
    "s32i a0, a1, 0",
    "s32i a12, a1, 4",
    "s32i a13, a1, 8",
    "s32i a14, a1, 12",
    "s32i a15, a1, 16",
    "rsr.sar a15",
    "s32i a15, a1, 20",
    // Preserve the first argument register. On the initial transition it
    // carries the trampoline's entry address; on later transitions restoring
    // it keeps the call0 caller's argument scratch register observable.
    "s32i a2, a1, 24",
    "s32i a1, a2, 0",
    "l32i a1, a3, 0",
    "l32i a15, a1, 20",
    "wsr.sar a15",
    "l32i a12, a1, 4",
    "l32i a13, a1, 8",
    "l32i a14, a1, 12",
    "l32i a15, a1, 16",
    "l32i a2, a1, 24",
    "l32i a0, a1, 0",
    "addi a1, a1, 32",
    "jx a0",
    ".size xtensa_switch_to, . - xtensa_switch_to",
);

unsafe extern "C" {
    // Both arguments own a stack-pointer slot. This lets the switch primitive
    // record the outgoing SP and load the incoming SP without a second return
    // path or ABI-specific return-value convention.
    fn xtensa_switch_to(previous_stack_pointer: *mut usize, next_stack_pointer: *mut usize);
}

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
    pub stack_pointer: usize,
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
        if self.tasks.len() >= 12 || stack_size < 256 {
            return None;
        }
        let stack = memory::allocate_stack(stack_size)?;
        // A fresh task starts through the scheduler's trampoline. The saved
        // context follows xtensa_switch_to's layout: PC, callee-saved
        // registers, SAR, and the call0 A2 argument.
        let frame = unsafe { stack.as_mut_ptr().add(stack.len() - 256) as *mut usize };
        unsafe {
            frame.write(entry);
            frame.add(1).write(0);
            frame.add(2).write(0);
            frame.add(3).write(0);
            frame.add(4).write(0);
            frame.add(5).write(0); // SAR
            frame.add(6).write(entry);
        }
        self.tasks.push(Task {
            id: crate::arch::xtensa::esp32::runtime::next_task_id(),
            name: String::from(name),
            state: TaskState::Ready,
            entry,
            stack,
            stack_pointer: frame as usize,
            context: TaskContext::empty(),
        });
        self.tasks.last()
    }

    pub fn run(&mut self) -> ! {
        if self.tasks.is_empty() {
            loop {
                core::hint::spin_loop();
            }
        }

        // Start the first task. Returning here means that task yielded.
        let first = &mut self.tasks[0];
        first.state = TaskState::Running;
        CURRENT_TASK.store(first as *mut Task as usize, Ordering::Release);

        let mut previous_index = 0usize;
        loop {
            YIELD_REQUESTED.store(false, Ordering::Release);
            if self
                .tasks
                .iter()
                .all(|task| task.state == TaskState::Finished)
            {
                crate::arch::xtensa::esp32::runtime::panic_message("all ESP32 tasks finished");
            }

            let count = self.tasks.len();
            let index = (previous_index + 1) % count;
            let mut next = None;
            for offset in 0..count {
                let candidate = (index + offset) % count;
                if self.tasks[candidate].state != TaskState::Finished {
                    next = Some(candidate);
                    break;
                }
            }
            let index = next.expect("finished-only tasks checked above");
            previous_index = index;
            let task = &mut self.tasks[index];
            task.state = TaskState::Running;
            CURRENT_TASK.store(task as *mut Task as usize, Ordering::Release);
            unsafe {
                xtensa_switch_to(
                    core::ptr::addr_of_mut!(SCHEDULER_STACK_POINTER),
                    core::ptr::addr_of_mut!(task.stack_pointer),
                );
            }
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
    let task_pointer = CURRENT_TASK.load(Ordering::Acquire);
    if task_pointer == 0 {
        return;
    }
    let task = unsafe { &mut *(task_pointer as *mut Task) };
    unsafe {
        xtensa_switch_to(
            core::ptr::addr_of_mut!(task.stack_pointer),
            core::ptr::addr_of_mut!(SCHEDULER_STACK_POINTER),
        );
    }
}
