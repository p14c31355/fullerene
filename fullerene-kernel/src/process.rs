//! Process management module for Fullerene OS
//!
//! This module provides process creation, scheduling, and context switching
//! capabilities for user-space programs.

#![feature(naked_functions)]

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::{PhysAddr, VirtAddr};

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
#[derive(Debug, Clone, Copy)]
pub struct ProcessContext {
    /// General purpose registers
    pub(crate) rax: u64,
    pub(crate) rbx: u64,
    pub(crate) rcx: u64,
    pub(crate) rdx: u64,
    pub(crate) rsi: u64,
    pub(crate) rdi: u64,
    pub(crate) rbp: u64,
    pub(crate) rsp: u64,
    pub(crate) r8: u64,
    pub(crate) r9: u64,
    pub(crate) r10: u64,
    pub(crate) r11: u64,
    pub(crate) r12: u64,
    pub(crate) r13: u64,
    pub(crate) r14: u64,
    pub(crate) r15: u64,

    /// CPU flags
    pub(crate) rflags: u64,

    /// Instruction pointer
    pub(crate) rip: u64,

    /// Segment registers
    pub(crate) cs: u64,
    pub(crate) ss: u64,
    pub(crate) ds: u64,
    pub(crate) es: u64,
    pub(crate) fs: u64,
    pub(crate) gs: u64,

    /// Task State Segment
    pub(crate) tss: u64,
}

impl Default for ProcessContext {
    fn default() -> Self {
        Self {
            rax: 0,
            rbx: 0,
            rcx: 0,
            rdx: 0,
            rsi: 0,
            rdi: 0,
            rbp: 0,
            rsp: 0,
            r8: 0,
            r9: 0,
            r10: 0,
            r11: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            rflags: 0x0202, // IF flag set
            rip: 0,
            cs: 0x08, // Kernel code segment
            ss: 0x10, // Kernel data segment
            ds: 0,
            es: 0,
            fs: 0,
            gs: 0,
            tss: 0,
        }
    }
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
    /// Process page table (physical address of level 4 page table)
    pub page_table_phys_addr: PhysAddr,
    /// Process page table mapper
    pub page_table: Option<crate::memory_management::ProcessPageTable>,
    /// Stack pointer for kernel stack
    pub kernel_stack: VirtAddr,
    /// User-space stack pointer
    pub user_stack: VirtAddr,
    /// Program entry point
    pub entry_point: VirtAddr,
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
            page_table_phys_addr: PhysAddr::new(0), // Will be set when allocated
            page_table: None,
            kernel_stack: VirtAddr::new(0), // Will be set when allocated
            user_stack: VirtAddr::new(0),   // Will be set when allocated
            entry_point: VirtAddr::new(entry_point as usize as u64),
            exit_code: None,
        }
    }

    /// Initialize process context for first execution
    pub fn init_context(&mut self, kernel_stack_top: VirtAddr) {
        self.context.rsp = kernel_stack_top.as_u64();
        // Set RIP to entry point through a trampoline that calls the function
        self.context.rip = process_trampoline as u64;
        // Store entry point in RAX for trampoline
        self.context.rax = self.entry_point.as_u64();
        self.context.rflags = 0x202; // Set Interrupt Enable flag

        // TODO: Use GDT constants instead of magic numbers
        self.context.cs = 0x1B; // User code selector with RPL=3
        self.context.ss = 0x23; // User data selector with RPL=3

        self.kernel_stack = kernel_stack_top;
    }
}

/// Global process list
pub static PROCESS_LIST: Mutex<Vec<Box<Process>>> = Mutex::new(Vec::new());

/// Next process to schedule (for round-robin)
static CURRENT_PROCESS_INDEX: Mutex<usize> = Mutex::new(0);

/// Current running process
static CURRENT_PROCESS: Mutex<Option<ProcessId>> = Mutex::new(None);

/// Kernel stack size per process (4KB)
const KERNEL_STACK_SIZE: usize = 4096;

/// Trampoline function to call process entry point
#[unsafe(naked)]
extern "C" fn process_trampoline() -> ! {
    // The entry point function pointer is stored in RAX by context switch
    core::arch::naked_asm!("call rax");
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

/// Perform context switch between two processes
pub unsafe fn context_switch(old_pid: Option<ProcessId>, new_pid: ProcessId) {
    use crate::context_switch::switch_context;

    // Get raw pointers to contexts to avoid holding lock during switch
    let mut process_list = PROCESS_LIST.lock();

    let old_proc_ptr = old_pid
        .and_then(|pid| process_list.iter_mut().find(|p| p.id == pid))
        .map(|p| &mut **p as *mut Process);

    let new_proc_ptr = process_list
        .iter()
        .find(|p| p.id == new_pid)
        .map(|p| &**p as *const Process);

    if let Some(new_ptr) = new_proc_ptr {
        let old_context = old_proc_ptr.map(|p| &mut (*p).context);
        let new_context = &(*new_ptr).context;

        // Drop the lock before the context switch to prevent deadlocks during timer interrupts
        drop(process_list);

        switch_context(old_context, new_context);
    }
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
