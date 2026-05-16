//! Process management module for Fullerene OS
//!
//! This module provides process creation, scheduling, and context switching
//! capabilities for user-space programs.

use alloc::boxed::Box;
use core::alloc::Layout;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use heapless::Vec;
use petroleum::common::logging::SystemError;
use petroleum::mem_debug;
use petroleum::page_table::PageTableHelper;
use spin::Mutex;
use x86_64::{PhysAddr, VirtAddr, registers::control::Cr3, structures::paging::PhysFrame};

/// Global lock for PROCESS_MANAGER (workaround for QEMU .bss not being zeroed)
static PM_LOCK: AtomicBool = AtomicBool::new(false);

/// Next available process ID
pub(crate) static NEXT_PID: AtomicUsize = AtomicUsize::new(1);

/// Maximum number of processes managed by the system
const MAX_PROCESSES: usize = 64;

/// Process ID type
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessId(pub u64);

impl core::fmt::Display for ProcessId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

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
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
pub struct ProcessContext {
    /// General purpose registers: rax, rbx, rcx, rdx, rsi, rdi, rbp, rsp, r8-r15
    pub(crate) regs: [u64; 16],
    /// CPU flags
    pub(crate) rflags: u64,
    /// Instruction pointer
    pub(crate) rip: u64,
    /// Segment registers: cs, ss, ds, es, fs, gs
    pub(crate) segments: [u64; 6],
    /// Task State Segment
    pub(crate) tss: u64,
    /// Whether the process runs in user mode (Ring 3)
    pub(crate) is_user: bool,
}

impl Default for ProcessContext {
    fn default() -> Self {
        Self {
            regs: [0; 16],
            rflags: 0x0202, // IF flag set
            rip: 0,
            segments: [
                crate::gdt::kernel_code_selector().0 as u64, // cs
                crate::gdt::kernel_code_selector().0 as u64, // ss
                0,
                0,
                0,
                0, // ds, es, fs, gs
            ],
            tss: 0,
            is_user: false,
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
    pub context: Box<ProcessContext>,
    /// Process page table (physical address of level 4 page table)
    pub page_table_phys_addr: PhysAddr,
    /// Process page table mapper
    pub page_table: Option<Box<petroleum::page_table::process::ProcessPageTable>>,
    /// Stack pointer for kernel stack
    pub kernel_stack: VirtAddr,
    /// User-space stack pointer
    pub user_stack: VirtAddr,
    /// Program entry point
    pub entry_point: VirtAddr,
    /// Whether the process runs in user mode (Ring 3)
    pub is_user: bool,
    /// Exit code - used for signaling ChildProcessExited signal
    pub exit_code: Option<i32>,
    /// Parent process ID (for wait() and signal propagation)
    pub parent_id: Option<ProcessId>,
}

impl Process {
    /// Create a new process
    pub fn new(name: &'static str, entry_point: VirtAddr, is_user: bool) -> Self {
        let id = ProcessId(NEXT_PID.fetch_add(1, Ordering::Relaxed) as u64);

        Self {
            id,
            name,
            state: ProcessState::Ready,
            context: Box::new(ProcessContext::default()),
            page_table_phys_addr: PhysAddr::new(0), // Will be set when allocated
            page_table: None,
            kernel_stack: VirtAddr::new(0), // Will be set when allocated
            user_stack: VirtAddr::new(0),   // Will be set when allocated
            entry_point,
            is_user,
            exit_code: None,
            parent_id: None, // Will be set by fork
        }
    }

    /// Initialize process context for first execution
    pub fn init_context(&mut self, kernel_stack_top: VirtAddr) {
        self.kernel_stack = kernel_stack_top;
        self.context.is_user = self.is_user;

        if self.is_user {
            // For user processes, the context RSP should be the user stack
            self.context.regs[7] = self.user_stack.as_u64(); // rsp
            self.context.segments[0] = crate::gdt::user_code_selector().0 as u64; // cs
            self.context.segments[1] = crate::gdt::user_data_selector().0 as u64; // ss
        } else {
            // For kernel processes, the context RSP is the kernel stack
            self.context.regs[7] = kernel_stack_top.as_u64(); // rsp
            self.context.segments[0] = crate::gdt::kernel_code_selector().0 as u64; // cs
            self.context.segments[1] = crate::gdt::kernel_code_selector().0 as u64; // ss
        }

        // Set RIP to entry point directly, assuming it's an extern "C" function
        self.context.rip = self.entry_point.as_u64();
        self.context.regs[0] = 0; // rax: For C functions, RAX is return value, init to 0
        self.context.rflags = 0x202; // Set Interrupt Enable flag
    }
}

/// Manages the global list of processes with encapsulated locking
pub struct ProcessManager {
    processes: Mutex<Vec<(ProcessId, Box<Process>), MAX_PROCESSES>>,
}

impl ProcessManager {
    pub const fn new() -> Self {
        Self {
            processes: Mutex::new(Vec::new()),
        }
    }

    /// Adds a new process to the list
    pub fn add(&self, process: Box<Process>) -> Result<(), SystemError> {
        let mut processes = self.processes.lock();
        let pid = process.id;
        if processes.len() >= MAX_PROCESSES {
            return Err(SystemError::TooManyProcesses);
        }
        // Remove existing entry with same PID if present
        if let Some(pos) = processes.iter().position(|(id, _)| *id == pid) {
            let _ = processes.swap_remove(pos);
        }
        processes
            .push((pid, process))
            .map_err(|_| SystemError::TooManyProcesses)
    }

    /// Performs an operation on a process found by PID
    pub fn with_process<F, R>(&self, pid: ProcessId, f: F) -> Option<R>
    where
        F: FnOnce(&mut Process) -> R,
    {
        let mut processes = self.processes.lock();
        processes
            .iter_mut()
            .find(|(id, _)| *id == pid)
            .map(|(_, p)| f(p))
    }

    /// Performs an operation on the entire process list
    pub fn with_list<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut Vec<(ProcessId, Box<Process>), MAX_PROCESSES>) -> R,
    {
        let mut processes = self.processes.lock();
        f(&mut *processes)
    }

    /// Returns the total number of processes
    pub fn count(&self) -> usize {
        self.processes.lock().len()
    }

    /// Returns the number of active processes (Ready or Running)
    pub fn active_count(&self) -> usize {
        self.processes
            .lock()
            .iter()
            .filter(|(_, p)| p.state == ProcessState::Ready || p.state == ProcessState::Running)
            .count()
    }

    /// Removes terminated processes from the list
    pub fn cleanup(&self) {
        let mut processes = self.processes.lock();
        processes.retain(|(_, p)| p.state != ProcessState::Terminated);
    }
}

/// Global process manager
pub static PROCESS_MANAGER: ProcessManager = ProcessManager::new();

/// Next process to schedule (for round-robin)
static CURRENT_PROCESS_INDEX: Mutex<usize> = Mutex::new(0);

/// Current running process (0 means None)
pub static CURRENT_PROCESS: AtomicUsize = AtomicUsize::new(0);

// Use KERNEL_STACK_SIZE from crate::heap

/// Initialize process management system
pub fn init() {
    mem_debug!("Process: init start\n");

    // Create idle process
    let idle_addr = VirtAddr::new(idle_loop as *const () as usize as u64);
    let mut idle_process = Process::new("idle", idle_addr, false);
    idle_process.state = ProcessState::Running;

    PROCESS_MANAGER
        .add(Box::new(idle_process))
        .expect("Failed to add idle process");
    CURRENT_PROCESS.store(1, Ordering::SeqCst);

    mem_debug!("Process: init done\n");
}

/// Create a new process and add it to the process list
pub fn create_process(
    name: &'static str,
    entry_point_address: VirtAddr,
    is_user: bool,
) -> Result<ProcessId, petroleum::common::logging::SystemError> {
    mem_debug!("Process: create_process starting\n");

    let mut process = Process::new(name, entry_point_address, is_user);

    // Allocate kernel stack for the process
    let stack_layout = Layout::from_size_align(crate::heap::KERNEL_STACK_SIZE, 16).unwrap();
    let stack_ptr = petroleum::common::memory::allocate_layout(stack_layout)?;
    let kernel_stack_top = VirtAddr::new(stack_ptr as u64 + crate::heap::KERNEL_STACK_SIZE as u64);

    if is_user {
        // Allocate user stack for the process
        let user_stack_layout =
            Layout::from_size_align(crate::heap::KERNEL_STACK_SIZE, 16).unwrap();
        let user_stack_ptr = petroleum::common::memory::allocate_layout(user_stack_layout)
            .map_err(|e| {
                petroleum::common::memory::deallocate_layout(stack_ptr, stack_layout);
                e
            })?;
        process.user_stack =
            VirtAddr::new(user_stack_ptr as u64 + crate::heap::KERNEL_STACK_SIZE as u64);
    }

    // Create page table for the process
    let page_table = match crate::memory_management::create_process_page_table() {
        Ok(pt) => pt,
        Err(e) => {
            log::error!("Failed to create process page table: {:?}", e);
            petroleum::common::memory::deallocate_layout(stack_ptr, stack_layout);
            return Err(e);
        }
    };
    process.page_table_phys_addr = PhysAddr::new(page_table.current_page_table() as u64);
    process.page_table = Some(Box::new(page_table));

    process.init_context(kernel_stack_top);

    let pid = process.id;
    PROCESS_MANAGER.add(Box::new(process))?;

    mem_debug!("Process: create_process done\n");
    Ok(pid)
}

/// Unblock parent processes that are waiting for this child process
fn unblock_waiting_parents(child_pid: ProcessId) {
    let parent_to_unblock = PROCESS_MANAGER.with_list(|list| {
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
        unblock_process(parent_id);
    }
}

/// Terminate a process
pub fn terminate_process(pid: ProcessId, exit_code: i32) {
    PROCESS_MANAGER.with_process(pid, |process| {
        process.state = ProcessState::Terminated;
        process.exit_code = Some(exit_code);

        // Free resources
        let kernel_stack_base =
            process.kernel_stack.as_u64() - crate::heap::KERNEL_STACK_SIZE as u64;
        let layout = Layout::from_size_align(crate::heap::KERNEL_STACK_SIZE, 16).unwrap();
        petroleum::common::memory::deallocate_layout(kernel_stack_base as *mut u8, layout);

        // Properly free page table frames recursively
        if let Some(page_table) = process.page_table.take() {
            if let Some(pml4_frame) = page_table.pml4_frame() {
                drop(page_table);
                crate::memory_management::deallocate_process_page_table(pml4_frame);
            }
        }

        process.page_table = None;
    });

    // Unblock parent processes waiting for this child
    unblock_waiting_parents(pid);

    // If current process is terminating, schedule next
    let current_pid = CURRENT_PROCESS.load(Ordering::SeqCst);
    if current_pid == pid.0 as usize {
        schedule_next();
    }
}

/// Idle process loop
fn idle_loop() {
    loop {
        // Use pause for QEMU-friendliness instead of hlt
        // pause allows the CPU to enter a low-power state while remaining responsive to interrupts,
        // making it more suitable for virtualization environments like QEMU compared to hlt which
        // puts the CPU in a deeper sleep state that's harder for hypervisors to manage efficiently.
        petroleum::cpu_pause();
    }
}

/// Schedule next process (round-robin)
pub fn schedule_next() {
    petroleum::scheduler_log!("Starting process scheduling");

    PROCESS_MANAGER.with_list(|process_list| {
        petroleum::scheduler_log!(
            "Acquired process list lock, {} processes",
            process_list.len()
        );

        if process_list.is_empty() {
            petroleum::scheduler_log!("No processes in list, cannot schedule");
            return;
        }

        let current_index = *CURRENT_PROCESS_INDEX.lock();
        petroleum::scheduler_log!("Current index: {}", current_index);

        let mut next_index = current_index;
        let start_index = current_index;

        loop {
            next_index = (next_index + 1) % process_list.len();
            let proc = process_list[next_index].1.as_ref();
            petroleum::scheduler_log!(
                "Checking process at index {}, name: {}, state: {:?}",
                next_index,
                proc.name,
                proc.state
            );

            if proc.state == ProcessState::Ready {
                petroleum::scheduler_log!("Found ready process at index {}", next_index);
                break;
            }

            if next_index == start_index {
                petroleum::scheduler_log!(
                    "Wrapped around, all processes blocked or completed check"
                );
                if let Some(idle_idx) = process_list.iter().position(|(_, p)| p.name == "idle") {
                    next_index = idle_idx;
                    petroleum::scheduler_log!("Switching to idle process at index {}", idle_idx);
                } else {
                    petroleum::scheduler_log!("No idle process found, using first process");
                    next_index = 0;
                }
                break;
            }
        }

        *CURRENT_PROCESS_INDEX.lock() = next_index;
        let next_pid = process_list[next_index].1.id.0 as usize;
        CURRENT_PROCESS.store(next_pid, Ordering::SeqCst);
        petroleum::scheduler_log!(
            "Set current process index to {}, PID {}",
            next_index,
            next_pid
        );

        if current_index != next_index {
            if let Some((_, current)) = process_list.get_mut(current_index) {
                if current.state == ProcessState::Running {
                    current.state = ProcessState::Ready;
                    petroleum::scheduler_log!("Marked current process as ready");
                }
            }

            if let Some((_, next)) = process_list.get_mut(next_index) {
                next.state = ProcessState::Running;
                petroleum::scheduler_log!("Marked next process as running");
            }
        }
    });

    petroleum::scheduler_log!("Process scheduling completed");
}

/// Get current process ID
pub fn current_pid() -> Option<ProcessId> {
    let pid = CURRENT_PROCESS.load(Ordering::SeqCst);
    if pid == 0 {
        None
    } else {
        Some(ProcessId(pid as u64))
    }
}

/// Yield current process
pub fn yield_current() {
    let old_pid = current_pid();
    schedule_next();
    let new_pid = current_pid().expect("schedule_next failed to select a process");
    unsafe {
        context_switch(old_pid, new_pid);
    }
}

/// Perform context switch between two processes
pub unsafe fn context_switch(old_pid: Option<ProcessId>, new_pid: ProcessId) {
    use crate::context_switch::switch_context;

    let (old_context_ptr, new_context_ptr, new_page_table) = PROCESS_MANAGER.with_list(|list| {
        let old_ptr = old_pid
            .and_then(|pid| list.iter_mut().find(|(id, _)| *id == pid))
            .map(|(_, p)| p.as_mut() as *mut Process);

        let new_ptr = list
            .iter()
            .find(|(id, _)| *id == new_pid)
            .map(|(_, p)| p.as_ref() as *const Process);

        if let Some(new_ptr) = new_ptr {
            let old_ctx = old_ptr.map(|p| p as *mut ProcessContext);
            let new_ctx = new_ptr as *const ProcessContext;
            let pt = unsafe { (*new_ptr).page_table_phys_addr };
            (Some(old_ctx), Some(new_ctx), Some(pt))
        } else {
            (None, None, None)
        }
    });

    if let (Some(old_ctx), Some(new_ctx), Some(pt)) =
        (old_context_ptr, new_context_ptr, new_page_table)
    {
        unsafe {
            let new_frame = PhysFrame::containing_address(pt);
            let (current_frame, _) = Cr3::read();
            if new_frame != current_frame {
                Cr3::write(new_frame, x86_64::registers::control::Cr3Flags::empty());
            }
            switch_context(old_ctx.map(|p| unsafe { &mut *p }), unsafe { &(*new_ctx) });
        }
    }
}

/// Block current process
pub fn block_current() {
    let pid = current_pid().expect("block_current called with no current process");

    let found = PROCESS_MANAGER.with_process(pid, |process| {
        process.state = ProcessState::Blocked;
    });

    if found.is_some() {
        let old_pid = Some(pid);
        schedule_next();
        let new_pid =
            current_pid().expect("schedule_next failed to select a process after blocking");
        unsafe {
            context_switch(old_pid, new_pid);
        }
    } else {
        panic!(
            "State inconsistency: current PID {} not found in process list",
            pid
        );
    }
}

/// Unblock a process
pub fn unblock_process(pid: ProcessId) {
    PROCESS_MANAGER.with_process(pid, |process| {
        if process.state == ProcessState::Blocked {
            process.state = ProcessState::Ready;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_creation() {
        let addr = VirtAddr::new(0);
        let proc = Process::new("test", addr, false);
        assert_eq!(proc.name, "test");
        assert_eq!(proc.state, ProcessState::Ready);
    }

    #[test]
    fn test_process_counting() {
        init(); // Initialize the process list
        assert!(get_process_count() > 0);
        assert!(get_active_process_count() > 0);
    }
}

/// Get total number of processes in the system
pub fn get_process_count() -> usize {
    PROCESS_MANAGER.count()
}

/// Get number of active processes (ready or running)
pub fn get_active_process_count() -> usize {
    PROCESS_MANAGER.active_count()
}

/// Clean up terminated processes to free resources
pub fn cleanup_terminated_processes() {
    PROCESS_MANAGER.cleanup();
}
// Test process module containing the test user process functions

// Test process main function
pub fn test_process_main() {
    // Use syscall helpers for reduced code duplication
    let message = b"Hello from test user process!\n";
    petroleum::write(1, message); // stdout fd = 1

    // Get and print PID
    let pid = petroleum::getpid();
    petroleum::write(1, b"My PID is: ");
    let pid_str = alloc::format!("{}\n", pid);
    petroleum::write(1, pid_str.as_bytes());

    // Yield twice for demonstration
    petroleum::sleep();
    petroleum::sleep();

    // Exit process
    petroleum::exit(0);
}
