//! Process management module for Fullerene OS
//!
//! This module provides process creation, scheduling, and context switching
//! capabilities for user-space programs.

#![no_std]
#![feature(asm)]

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::{PhysAddr, VirtAddr};
use petroleum::common::{EfiMemoryDescriptor, EfiMemoryType};

/// Process ID type
pub type ProcessId = u64;

/// Process states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// Process is ready to run
    Ready,
    /// Process is currently running
    Running,
    /// Process is waiting for I/O or other event
    Blocked,
    /// Process has terminated
    Terminated,
}

/// Process context for context switching
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessContext {
    /// General purpose registers
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub rsp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,

    /// Instruction pointer
    pub rip: u64,

    /// CPU flags
    pub rflags: u64,

    /// Segment registers
    pub cs: u64,
    pub ss: u64,
    pub ds: u64,
    pub es: u64,
    pub fs: u64,
    pub gs: u64,

    /// Task State Segment
    pub tss: u64,
}



/// Process structure
pub struct Process {
    /// Unique process ID
    pub id: ProcessId,
    /// Process name
    pub name: &'static str,
    /// Current state
    pub state: ProcessState,
    /// CPU context for context switching
    pub context: ProcessContext,
    /// Process memory mapping
    pub page_table: PhysAddr,
    /// Stack pointer for kernel stack
    pub kernel_stack: VirtAddr,
    /// Entry point function
    pub entry_point: fn(),
    /// Exit code
    pub exit_code: Option<i32>,
}

impl Process {
    /// Create a new process
    pub fn new(name: &'static str, entry_point: fn()) -> Self {
        static NEXT_PID: AtomicU64 = AtomicU64::new(1);

        let id = NEXT_PID.fetch_add(1, Ordering::Relaxed);

        Self {
            id,
            name,
            state: ProcessState::Ready,
            context: ProcessContext::default(),
            page_table: PhysAddr::new(0), // Will be set when allocated
            kernel_stack: VirtAddr::new(0), // Will be set when allocated
            entry_point,
            exit_code: None,
        }
    }

    /// Initialize process context for first execution
    pub fn init_context(&mut self, kernel_stack_top: VirtAddr) {
        self.context.rsp = kernel_stack_top.as_u64();
        // Set RIP to entry point through a trampoline that calls the function
        self.context.rip = process_trampoline as u64;
        // Store entry point in RAX for trampoline
        self.context.rax = self.entry_point as u64;
        self.kernel_stack = kernel_stack_top;
    }
}

/// Global process list
static PROCESS_LIST: Mutex<Vec<Box<Process>>> = Mutex::new(Vec::new());

/// Next process to schedule (for round-robin)
static CURRENT_PROCESS_INDEX: Mutex<usize> = Mutex::new(0);

/// Current running process
static CURRENT_PROCESS: Mutex<Option<ProcessId>> = Mutex::new(None);

/// Kernel stack size per process (4KB)
const KERNEL_STACK_SIZE: usize = 4096;

/// Trampoline function to call process entry point
extern "C" fn process_trampoline() {
    // The entry point function pointer is stored in RAX by context switch
    // We can't use unsafe extern "C" fn() from naked fn, so this is a placeholder
    // Actual implementation will need assembly trampoline

    unsafe {
        // This will be replaced with proper assembly
        asm!(
            "call rax", // Call the function in RAX
            options(noreturn)
        );
    }
}

/// Initialize process management system
pub fn init() {
    // Create idle process
    let mut idle_process = Process::new("idle", idle_loop);
    idle_process.state = ProcessState::Running;

    let mut process_list = PROCESS_LIST.lock();
    process_list.push(Box::new(idle_process));

    // Set current process
    *CURRENT_PROCESS.lock() = Some(1);
}

/// Create a new process and add it to the process list
pub fn create_process(name: &'static str, entry_point: fn()) -> ProcessId {
    let mut process = Process::new(name, entry_point);

    // Allocate kernel stack for the process
    let stack_layout = Layout::from_size_align(KERNEL_STACK_SIZE, 16).unwrap();
    let stack_ptr = unsafe { alloc::alloc::alloc(stack_layout) };
    let kernel_stack_top = VirtAddr::new(stack_ptr as u64 + KERNEL_STACK_SIZE as u64);

    process.init_context(kernel_stack_top);

    let pid = process.id;
    let mut process_list = PROCESS_LIST.lock();
    process_list.push(Box::new(process));

    pid
}

/// Terminate a process
pub fn terminate_process(pid: ProcessId, exit_code: i32) {
    let mut process_list = PROCESS_LIST.lock();
    if let Some(process) = process_list.iter_mut().find(|p| p.id == pid) {
        process.state = ProcessState::Terminated;
        process.exit_code = Some(exit_code);
    }

    // If current process is terminating, schedule next
    let current_pid = *CURRENT_PROCESS.lock();
    if current_pid == Some(pid) {
        schedule_next();
    }
}

/// Idle process loop
fn idle_loop() {
    loop {
        unsafe {
            x86_64::instructions::hlt();
        }
    }
}

/// Schedule next process (round-robin)
pub fn schedule_next() {
    let mut process_list = PROCESS_LIST.lock();
    let current_index = *CURRENT_PROCESS_INDEX.lock();

    // Find next ready process
    let mut next_index = current_index;
    loop {
        next_index = (next_index + 1) % process_list.len();
        if process_list[next_index].state == ProcessState::Ready {
            break;
        }
        if next_index == current_index {
            // All processes blocked, run idle
            if let Some(idle) = process_list.iter().find(|p| p.name == "idle") {
                next_index = process_list.iter().position(|p| p.id == idle.id).unwrap();
            }
            break;
        }
    }

    // Update current process tracking
    *CURRENT_PROCESS_INDEX.lock() = next_index;
    *CURRENT_PROCESS.lock() = Some(process_list[next_index].id);

    // Mark current as ready, next as running
    if current_index != next_index {
        if let Some(current) = process_list.get_mut(current_index) {
            if current.state == ProcessState::Running {
                current.state = ProcessState::Ready;
            }
        }

        if let Some(next) = process_list.get_mut(next_index) {
            next.state = ProcessState::Running;
        }
    }
}

/// Get current process ID
pub fn current_pid() -> Option<ProcessId> {
    *CURRENT_PROCESS.lock()
}

/// Yield current process
pub fn yield_current() {
    schedule_next();
    // Context switch would happen here
    // For now, this just moves to next process in the list
}

/// Block current process
pub fn block_current() {
    let current_pid = current_pid().unwrap();
    let mut process_list = PROCESS_LIST.lock();

    if let Some(process) = process_list.iter_mut().find(|p| p.id == current_pid) {
        process.state = ProcessState::Blocked;
    }

    schedule_next();
}

/// Unblock a process
pub fn unblock_process(pid: ProcessId) {
    let mut process_list = PROCESS_LIST.lock();
    if let Some(process) = process_list.iter_mut().find(|p| p.id == pid) {
        if process.state == ProcessState::Blocked {
            process.state = ProcessState::Ready;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_creation() {
        let mut proc = Process::new("test", || {});
        assert_eq!(proc.name, "test");
        assert_eq!(proc.state, ProcessState::Ready);
    }
}
