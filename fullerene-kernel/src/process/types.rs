use super::*;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use alloc::boxed::Box;
use alloc::vec::Vec;
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
            rax: 0, rbx: 0, rcx: 0, rdx: 0, rsi: 0, rdi: 0, rbp: 0, rsp: 0,
            r8: 0, r9: 0, r10: 0, r11: 0, r12: 0, r13: 0, r14: 0, r15: 0,
            rflags: 0x0202, rip: 0,
            cs: crate::gdt::kernel_code_selector().0 as u64,
            ss: crate::gdt::kernel_code_selector().0 as u64,
            ds: 0, es: 0, fs: 0, gs: 0, tss: 0,
        }
    }
}

/// Process structure
#[repr(C)]
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
    /// Parent process ID (for wait() mechanism)
    pub parent_id: Option<ProcessId>,
}

/// Global process list
pub static PROCESS_LIST: Mutex<Vec<Box<Process>>> = Mutex::new(Vec::new());

/// Next process to schedule (for round-robin)
static CURRENT_PROCESS_INDEX: Mutex<usize> = Mutex::new(0);

/// Current running process
pub static CURRENT_PROCESS: Mutex<Option<ProcessId>> = Mutex::new(None);

/// Kernel stack size per process (4KB)
pub const KERNEL_STACK_SIZE: usize = 4096;

/// Trampoline function to call process entry point
#[unsafe(naked)]
extern "C" fn process_trampoline() -> ! {
    unsafe { core::arch::naked_asm!("jmp rax") };
}

impl Process {
    /// Create a new process
    pub fn new(name: &'static str, entry_point: VirtAddr) -> Self {
        static NEXT_PID: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_PID.fetch_add(1, Ordering::Relaxed);
        Self {
            id, name, state: ProcessState::Ready, context: ProcessContext::default(),
            page_table_phys_addr: PhysAddr::new(0), page_table: None,
            kernel_stack: VirtAddr::new(0), user_stack: VirtAddr::new(0),
            entry_point, exit_code: None, parent_id: None,
        }
    }

    /// Initialize process context for first execution
    pub fn init_context(&mut self, kernel_stack_top: VirtAddr) {
        self.context.rsp = kernel_stack_top.as_u64();
        self.context.rip = process_trampoline as u64;
        self.context.rax = self.entry_point.as_u64();
        self.context.rflags = 0x202;
        self.context.cs = crate::gdt::user_code_selector().0 as u64;
        self.context.ss = crate::gdt::user_data_selector().0 as u64;
        self.kernel_stack = kernel_stack_top;
    }
}
