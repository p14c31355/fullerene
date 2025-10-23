//! Process management module for Fullerene OS
//!
//! This module provides process creation, scheduling, and context switching
//! capabilities for user-space programs.

use crate::errors::SystemError;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::sync::atomic::{AtomicU64, Ordering};
use petroleum::{page_table::PageTableHelper, write_serial_bytes};
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
            cs: crate::gdt::kernel_code_selector().0 as u64,
            ss: crate::gdt::kernel_code_selector().0 as u64, // Kernel data same as code for ring 0
            // But since init_context overrides, and Default may be used sparingly, keep existing.
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
    /// Exit code - used for signaling ChildProcessExited signal
    pub exit_code: Option<i32>,
    /// Parent process ID (for wait() and signal propagation)
    pub parent_id: Option<ProcessId>,
}

impl Process {
    /// Create a new process
    pub fn new(name: &'static str, entry_point: VirtAddr) -> Self {
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
            entry_point,
            exit_code: None,
            parent_id: None, // Will be set by fork
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

        self.context.cs = crate::gdt::user_code_selector().0 as u64;
        self.context.ss = crate::gdt::user_data_selector().0 as u64;

        self.kernel_stack = kernel_stack_top;
    }
}

/// Global process list
pub static PROCESS_LIST: Mutex<Vec<Box<Process>>> = Mutex::new(Vec::new());

/// Next process to schedule (for round-robin)
static CURRENT_PROCESS_INDEX: Mutex<usize> = Mutex::new(0);

/// Current running process
pub static CURRENT_PROCESS: Mutex<Option<ProcessId>> = Mutex::new(None);

/// Kernel stack size per process (4KB)
const KERNEL_STACK_SIZE: usize = 4096;

/// Trampoline function to call process entry point
#[unsafe(naked)]
extern "C" fn process_trampoline() -> ! {
    // The entry point function pointer is stored in RAX by context switch
    core::arch::naked_asm!("jmp rax");
}

/// Initialize process management system
pub fn init() {
    // Create idle process
    let idle_addr = VirtAddr::new(idle_loop as usize as u64);
    let mut idle_process = Process::new("idle", idle_addr);
    idle_process.state = ProcessState::Running;

    petroleum::lock_and_modify!(PROCESS_LIST, process_list, {
        process_list.push(Box::new(idle_process));
    });

    // Set current process
    petroleum::lock_and_modify!(CURRENT_PROCESS, current_proc, {
        *current_proc = Some(1);
    });
}

/// Create a new process and add it to the process list
pub fn create_process(
    name: &'static str,
    entry_point_address: VirtAddr,
) -> Result<ProcessId, petroleum::common::logging::SystemError> {
    write_serial_bytes!(0x3F8, 0x3FD, b"Process: create_process starting\n");

    let mut process = Process::new(name, entry_point_address);
    write_serial_bytes!(0x3F8, 0x3FD, b"Process: Process::new done\n");

    // Allocate kernel stack for the process
    let stack_layout = Layout::from_size_align(KERNEL_STACK_SIZE, 16).unwrap();
    let stack_ptr = unsafe { alloc::alloc::alloc(stack_layout) };
    if stack_ptr.is_null() {
        return Err(petroleum::common::logging::SystemError::MemOutOfMemory);
    }
    let kernel_stack_top = VirtAddr::new(stack_ptr as u64 + KERNEL_STACK_SIZE as u64);
    write_serial_bytes!(0x3F8, 0x3FD, b"Process: Kernel stack allocated\n");

    // Create page table for the process
    let page_table = match crate::memory_management::create_process_page_table() {
        Ok(pt) => pt,
        Err(e) => {
            log::error!("Failed to create process page table: {:?}", e);
            // Deallocate stack to prevent memory leak on error
            unsafe {
                alloc::alloc::dealloc(stack_ptr, stack_layout);
            }
            return Err(e);
        }
    };
    process.page_table_phys_addr = PhysAddr::new(page_table.current_page_table() as u64);
    process.page_table = Some(page_table);

    process.init_context(kernel_stack_top);
    write_serial_bytes!(0x3F8, 0x3FD, b"Process: Context initialized\n");

    let pid = process.id;
    let mut process_list = PROCESS_LIST.lock();
    process_list.push(Box::new(process));
    write_serial_bytes!(0x3F8, 0x3FD, b"Process: Process added to list\n");

    Ok(pid)
}

/// Unblock parent processes that are waiting for this child process
fn unblock_waiting_parents(child_pid: ProcessId) {
    let process_list = PROCESS_LIST.lock();

    // Find the parent of the terminated process
    if let Some(terminated_proc) = process_list.iter().find(|p| p.id == child_pid) {
        if let Some(parent_id) = terminated_proc.parent_id {
            // Find the parent process and unblock it if it's blocked waiting
            if let Some(parent) = process_list.iter().find(|p| p.id == parent_id) {
                if parent.state == ProcessState::Blocked {
                    // Note: In a full implementation, we'd signal that the child has terminated
                    // For now, just unblock the parent so it can continue executing
                    drop(process_list);
                    unblock_process(parent_id);
                }
            }
        }
    }
}

/// Terminate a process
pub fn terminate_process(pid: ProcessId, exit_code: i32) {
    let mut process_list = PROCESS_LIST.lock();
    if let Some(process) = process_list.iter_mut().find(|p| p.id == pid) {
        process.state = ProcessState::Terminated;
        process.exit_code = Some(exit_code);

        // Free resources
        let kernel_stack_base = process.kernel_stack.as_u64() - KERNEL_STACK_SIZE as u64;
        let layout = Layout::from_size_align(KERNEL_STACK_SIZE, 16).unwrap();
        unsafe { alloc::alloc::dealloc(kernel_stack_base as *mut u8, layout) };

        // Properly free page table frames recursively
        if let Some(page_table) = process.page_table.take() {
            // For now, skip deallocation if no allocated pml4_frame
            // This handles the case where page table was created with current CR3 (fallback)
            if let Some(pml4_frame) = page_table.pml4_frame() {
                drop(page_table); // Explicit drop to release the mapper
                crate::memory_management::deallocate_process_page_table(pml4_frame);
            }
        }

        process.page_table = None; // Already taken above, this is redundant but safe

        // Unblock parent processes waiting for this child
        unblock_waiting_parents(pid);
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
        // Use pause for QEMU-friendliness instead of hlt
        // pause allows the CPU to enter a low-power state while remaining responsive to interrupts,
        // making it more suitable for virtualization environments like QEMU compared to hlt which
        // puts the CPU in a deeper sleep state that's harder for hypervisors to manage efficiently.
        petroleum::cpu_pause();
    }
}

/// Schedule next process (round-robin)
pub fn schedule_next() {
    log::info!("Schedule_next: Starting process scheduling");

    let mut process_list = PROCESS_LIST.lock();
    log::info!(
        "Schedule_next: Acquired process list lock, {} processes",
        process_list.len()
    );

    // Handle empty process list
    if process_list.is_empty() {
        log::info!("Schedule_next: No processes in list, cannot schedule");
        return;
    }

    let current_index = *CURRENT_PROCESS_INDEX.lock();
    log::info!("Schedule_next: Current index: {}", current_index);

    // Find next ready process
    let mut next_index = current_index;
    let start_index = current_index;
    let mut found_ready = false;

    loop {
        next_index = (next_index + 1) % process_list.len();
        log::info!(
            "Schedule_next: Checking process at index {}, name: {}, state: {:?}",
            next_index,
            process_list[next_index].name,
            process_list[next_index].state
        );

        if process_list[next_index].state == ProcessState::Ready {
            log::info!("Schedule_next: Found ready process at index {}", next_index);
            found_ready = true;
            break;
        }

        if next_index == start_index {
            log::info!("Schedule_next: Wrapped around, all processes blocked or completed check");
            // All processes blocked, run idle
            if let Some(idle_idx) = process_list.iter().position(|p| p.name == "idle") {
                next_index = idle_idx;
                log::info!(
                    "Schedule_next: Switching to idle process at index {}",
                    idle_idx
                );
            } else {
                log::info!("Schedule_next: No idle process found, using first process");
                next_index = 0;
            }
            break;
        }
    }

    // Update current process tracking
    *CURRENT_PROCESS_INDEX.lock() = next_index;
    *CURRENT_PROCESS.lock() = Some(process_list[next_index].id);
    log::info!(
        "Schedule_next: Set current process index to {}, PID {}",
        next_index,
        process_list[next_index].id
    );

    // Mark current as ready, next as running
    if current_index != next_index {
        if let Some(current) = process_list.get_mut(current_index) {
            if current.state == ProcessState::Running {
                current.state = ProcessState::Ready;
                log::info!("Schedule_next: Marked current process as ready");
            }
        }

        if let Some(next) = process_list.get_mut(next_index) {
            next.state = ProcessState::Running;
            log::info!("Schedule_next: Marked next process as running");
        }
    }

    log::info!("Schedule_next: Process scheduling completed");
}

/// Get current process ID
pub fn current_pid() -> Option<ProcessId> {
    petroleum::lock_and_read!(CURRENT_PROCESS, proc, *proc)
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

    let mut process_list = PROCESS_LIST.lock();

    let old_proc_ptr = old_pid
        .and_then(|pid| process_list.iter_mut().find(|p| p.id == pid))
        .map(|p| p.as_mut() as *mut Process);

    let new_proc_ptr = process_list
        .iter()
        .find(|p| p.id == new_pid)
        .map(|p| p.as_ref() as *const Process);

    if let Some(new_ptr) = new_proc_ptr {
        let old_context = old_proc_ptr.map(|p| unsafe { &mut (*p).context });
        let new_context = unsafe { &(*new_ptr).context };

        // Drop the lock before the context switch to prevent deadlocks.
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
        let addr = VirtAddr::new(0);
        let proc = Process::new("test", addr);
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
    PROCESS_LIST.lock().len()
}

/// Get number of active processes (ready or running)
pub fn get_active_process_count() -> usize {
    PROCESS_LIST
        .lock()
        .iter()
        .filter(|p| p.state == ProcessState::Ready || p.state == ProcessState::Running)
        .count()
}

/// Clean up terminated processes to free resources
pub fn cleanup_terminated_processes() {
    let mut process_list = PROCESS_LIST.lock();

    // Remove terminated processes from the list. This will drop the `Box<Process>`,
    // freeing the memory for the struct itself. `terminate_process` should have
    // already been called to free other associated resources.
    process_list.retain(|p| p.state != ProcessState::Terminated);
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
