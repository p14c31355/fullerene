//! Process management module for Fullerene OS
//!
//! This module provides process creation, scheduling, and context switching
//! capabilities for user-space programs.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::sync::atomic::{AtomicUsize, Ordering};
use petroleum::common::logging::SystemError;
use petroleum::debug_log;
use petroleum::page_table::PageTableHelper;
use spin::Mutex;
use x86_64::{
    registers::control::Cr3,
    structures::paging::PhysFrame,
    PhysAddr, VirtAddr,
};

/// Next available process ID
pub(crate) static NEXT_PID: AtomicUsize = AtomicUsize::new(1);

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
    /// Whether the process runs in user mode (Ring 3)
    pub(crate) is_user: bool,
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
        let id = NEXT_PID.fetch_add(1, Ordering::Relaxed) as u64;

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
            self.context.rsp = self.user_stack.as_u64();
            self.context.cs = crate::gdt::user_code_selector().0 as u64;
            self.context.ss = crate::gdt::user_data_selector().0 as u64;
        } else {
            // For kernel processes, the context RSP is the kernel stack
            self.context.rsp = kernel_stack_top.as_u64();
            self.context.cs = crate::gdt::kernel_code_selector().0 as u64;
            self.context.ss = crate::gdt::kernel_code_selector().0 as u64;
        }

        // Set RIP to entry point directly, assuming it's an extern "C" function
        self.context.rip = self.entry_point.as_u64();
        self.context.rax = 0; // For C functions, RAX is return value, init to 0
        self.context.rflags = 0x202; // Set Interrupt Enable flag
    }
}

/// Manages the global list of processes with encapsulated locking
pub struct ProcessManager {
    list: Mutex<[Option<Process>; 16]>,
}

impl ProcessManager {
    pub const fn new() -> Self {
        const NONE: Option<Process> = None;
        Self {
            list: Mutex::new([NONE; 16]),
        }
    }

    /// Adds a new process to the list
    pub fn add(&self, process: Process) {
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [ProcessManager::add] attempting lock\n");
        let mut list = self.list.lock();
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [ProcessManager::add] lock acquired\n");
        
        let pid = process.id;
        let index = (pid as usize % 16);
        
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [ProcessManager::add] attempting insert\n");
        list[index] = Some(process);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [ProcessManager::add] insert complete\n");
    }

    /// Performs an operation on a process found by PID
    pub fn with_process<F, R>(&self, pid: ProcessId, f: F) -> Option<R>
    where
        F: FnOnce(&mut Process) -> R,
    {
        let mut list = self.list.lock();
        let index = (pid as usize % 16);
        list[index].as_mut().filter(|p| p.id == pid).map(f)
    }

    /// Performs an operation on the entire process list
    pub fn with_list<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut [Option<Process>; 16]) -> R,
    {
        let mut list = self.list.lock();
        f(&mut *list)
    }

    /// Returns the total number of processes
    pub fn count(&self) -> usize {
        self.list.lock().iter().filter(|p| p.is_some()).count()
    }

    /// Returns the number of active processes (Ready or Running)
    pub fn active_count(&self) -> usize {
        self.list
            .lock()
            .iter()
            .flatten()
            .filter(|p| p.state == ProcessState::Ready || p.state == ProcessState::Running)
            .count()
    }

    /// Removes terminated processes from the list
    pub fn cleanup(&self) {
        let mut list = self.list.lock();
        for proc_opt in list.iter_mut() {
            if let Some(proc) = proc_opt {
                if proc.state == ProcessState::Terminated {
                    *proc_opt = None;
                }
            }
        }
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
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [process::init] start\n");
    
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [process::init] testing raw buffer write\n");
    unsafe {
        crate::heap::BOOT_HEAP_BUFFER.0[0] = 0xAA;
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [process::init] raw buffer write SUCCESS\n");
    }

    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [process::init] testing IDT with int3\n");
    x86_64::instructions::interrupts::int3();
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [process::init] int3 returned\n");

    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [process::init] testing heap allocation (8 bytes)\n");
    let layout8 = Layout::from_size_align(8, 8).unwrap();
    unsafe {
        let ptr8 = alloc::alloc::alloc(layout8);
        if ptr8.is_null() {
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [process::init] 8 bytes allocation FAILED\n");
        } else {
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [process::init] 8 bytes allocation SUCCESS\n");
            alloc::alloc::dealloc(ptr8, layout8);
        }
    }

    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [process::init] testing heap allocation (1024 bytes)\n");
    let layout1k = Layout::from_size_align(1024, 16).unwrap();
    unsafe {
        let ptr1k = alloc::alloc::alloc(layout1k);
        if ptr1k.is_null() {
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [process::init] 1024 bytes allocation FAILED\n");
        } else {
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [process::init] 1024 bytes allocation SUCCESS\n");
            alloc::alloc::dealloc(ptr1k, layout1k);
        }
    }

    // Create idle process
    let idle_addr = VirtAddr::new(idle_loop as usize as u64);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [process::init] creating idle process\n");
    let mut idle_process = Process::new("idle", idle_addr, false);
    idle_process.state = ProcessState::Running;

    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [process::init] adding idle process to manager\n");
    PROCESS_MANAGER.add(idle_process);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [process::init] idle process added\n");

    // Set current process
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [process::init] setting CURRENT_PROCESS\n");
    CURRENT_PROCESS.store(1, Ordering::SeqCst);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [process::init] CURRENT_PROCESS modified\n");
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [process::init] done\n");
}

/// Create a new process and add it to the process list
pub fn create_process(
    name: &'static str,
    entry_point_address: VirtAddr,
    is_user: bool,
) -> Result<ProcessId, petroleum::common::logging::SystemError> {
    debug_log!("Process: create_process starting");

    let mut process = Process::new(name, entry_point_address, is_user);
    debug_log!("Process: Process::new done");

    // Allocate kernel stack for the process
    let stack_layout = Layout::from_size_align(crate::heap::KERNEL_STACK_SIZE, 16).unwrap();
    let stack_ptr = unsafe { alloc::alloc::alloc(stack_layout) };
    if stack_ptr.is_null() {
        return Err(petroleum::common::logging::SystemError::MemOutOfMemory);
    }
    let kernel_stack_top = VirtAddr::new(stack_ptr as u64 + crate::heap::KERNEL_STACK_SIZE as u64);
    debug_log!("Process: Kernel stack allocated");

    if is_user {
        // Allocate user stack for the process
        let user_stack_layout = Layout::from_size_align(crate::heap::KERNEL_STACK_SIZE, 16).unwrap();
        let user_stack_ptr = unsafe { alloc::alloc::alloc(user_stack_layout) };
        if user_stack_ptr.is_null() {
            unsafe { alloc::alloc::dealloc(stack_ptr, stack_layout) };
            return Err(petroleum::common::logging::SystemError::MemOutOfMemory);
        }
        process.user_stack = VirtAddr::new(user_stack_ptr as u64 + crate::heap::KERNEL_STACK_SIZE as u64);
        debug_log!("Process: User stack allocated");
    }

    debug_log!("Process: Creating page table...");
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
    debug_log!("Process: Page table created");
    process.page_table_phys_addr = PhysAddr::new(page_table.current_page_table() as u64);
    process.page_table = Some(page_table);

    process.init_context(kernel_stack_top);
    debug_log!("Process: Context initialized");

    let pid = process.id;
    debug_log!("Process: Adding process to manager...");
    PROCESS_MANAGER.add(process);
    debug_log!("Process: Process added to list");

    Ok(pid)
}

/// Unblock parent processes that are waiting for this child process
fn unblock_waiting_parents(child_pid: ProcessId) {
    let parent_to_unblock = PROCESS_MANAGER.with_list(|list| {
        list.iter()
            .flatten()
            .find(|p| p.id == child_pid)
            .and_then(|terminated_proc| terminated_proc.parent_id)
            .filter(|&parent_id| {
                list.iter()
                    .flatten()
                    .find(|p| p.id == parent_id)
                    .map_or(false, |parent| parent.state == ProcessState::Blocked)
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
        let kernel_stack_base = process.kernel_stack.as_u64() - crate::heap::KERNEL_STACK_SIZE as u64;
        let layout = Layout::from_size_align(crate::heap::KERNEL_STACK_SIZE, 16).unwrap();
        unsafe { alloc::alloc::dealloc(kernel_stack_base as *mut u8, layout) };

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
    if current_pid == pid as usize {
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
            let proc = process_list[next_index].as_ref();
            petroleum::scheduler_log!(
                "Checking process at index {}, name: {}, state: {:?}",
                next_index,
                proc.map(|p| p.name).unwrap_or("None"),
                proc.map(|p| p.state).unwrap_or(ProcessState::Terminated)
            );

            if proc.map(|p| p.state == ProcessState::Ready).unwrap_or(false) {
                petroleum::scheduler_log!("Found ready process at index {}", next_index);
                break;
            }

            if next_index == start_index {
                petroleum::scheduler_log!("Wrapped around, all processes blocked or completed check");
                if let Some(idle_idx) = process_list.iter().position(|p| p.as_ref().map(|p| p.name == "idle").unwrap_or(false)) {
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
        let next_pid = process_list[next_index].as_ref().map(|p| p.id as usize).unwrap_or(0);
        CURRENT_PROCESS.store(next_pid, Ordering::SeqCst);
        petroleum::scheduler_log!(
            "Set current process index to {}, PID {}",
            next_index,
            next_pid
        );

        if current_index != next_index {
            if let Some(current_opt) = process_list.get_mut(current_index) {
                if let Some(current) = current_opt.as_mut() {
                    if current.state == ProcessState::Running {
                        current.state = ProcessState::Ready;
                        petroleum::scheduler_log!("Marked current process as ready");
                    }
                }
            }

            if let Some(next_opt) = process_list.get_mut(next_index) {
                if let Some(next) = next_opt.as_mut() {
                    next.state = ProcessState::Running;
                    petroleum::scheduler_log!("Marked next process as running");
                }
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
        Some(pid as ProcessId)
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
            .and_then(|pid| {
                let index = (pid as usize % 16);
                list[index].as_mut().filter(|p| p.id == pid)
            })
            .map(|p| p as *mut Process);

        let index = (new_pid as usize % 16);
        let new_ptr = list[index].as_ref().filter(|p| p.id == new_pid).map(|p| p as *const Process);

        if let Some(new_ptr) = new_ptr {
            let old_ctx = old_ptr.map(|p| p as *mut ProcessContext);
            let new_ctx = new_ptr as *const ProcessContext;
            let pt = unsafe { (*new_ptr).page_table_phys_addr };
            (Some(old_ctx), Some(new_ctx), Some(pt))
        } else {
            (None, None, None)
        }
    });

    if let (Some(old_ctx), Some(new_ctx), Some(pt)) = (old_context_ptr, new_context_ptr, new_page_table) {
        unsafe {
            let new_frame = PhysFrame::containing_address(pt);
            let (current_frame, _) = Cr3::read();
            if new_frame != current_frame {
                Cr3::write(new_frame, x86_64::registers::control::Cr3Flags::empty());
            }
            switch_context(
                old_ctx.map(|p| unsafe { &mut *p }),
                unsafe { &(*new_ctx) },
            );
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
