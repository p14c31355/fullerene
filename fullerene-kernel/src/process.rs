//! Process management module for Fullerene OS
//!
//! This module provides process creation, scheduling, and context switching
//! capabilities for user-space programs.

use alloc::boxed::Box;
use alloc::collections::btree_map::BTreeMap;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::sync::atomic::{AtomicUsize, Ordering};
use heapless::Vec as HeaplessVec;
use petroleum::common::logging::SystemError;
use petroleum::mem_debug;
use petroleum::page_table::PageTableHelper as _;
use spin::Mutex;
use x86_64::{PhysAddr, VirtAddr, registers::control::Cr3, structures::paging::PhysFrame};

use crate::linux::runtime::DispatchMode;
use crate::vdso::{VdsoPageRef, create_vdso_page};

use crate::syscall::{Handle, HandlePerms, KernelObject};

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
                // Use fallback segment selectors if GDT not ready
                crate::gdt::code()
                    .as_ref()
                    .map_or(1, |s| s.0 as u64), // cs
                crate::gdt::kernel_data()
                    .as_ref()
                    .map_or(2, |s| s.0 as u64), // ss
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

/// Per-process file descriptor table.
pub struct FdTable {
    pub entries: BTreeMap<u32, crate::fs::FileDesc>,
    pub next_fd: u32,
}

impl FdTable {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            next_fd: 3,
        }
    }
}

/// A slot entry in the per-process handle table.
struct HandleEntry {
    generation: u16,
    permissions: u8,
    object: KernelObject,
}

/// A slot that retains its generation across free/alloc cycles for
/// stale-handle (use-after-free) protection.
struct HandleSlot {
    generation: u16,
    entry: Option<HandleEntry>,
}

/// Per-process handle table using slot-based allocation with generation counters.
pub struct HandleTable {
    slots: alloc::vec::Vec<HandleSlot>,
    #[allow(dead_code)]
    free_head: Option<u16>,
    #[allow(dead_code)]
    capacity: u16,
}

impl HandleTable {
    pub fn new() -> Self {
        Self {
            slots: alloc::vec::Vec::new(),
            free_head: None,
            capacity: 1024,
        }
    }

    /// Allocate a new handle slot. Returns the Handle with owner-default permissions.
    pub fn alloc(&mut self, object: KernelObject) -> Handle {
        let slot_idx = self.find_free_slot();
        let slot = &mut self.slots[slot_idx as usize];
        slot.generation = slot.generation.wrapping_add(1);
        let perms = (HandlePerms::READ
            | HandlePerms::WRITE
            | HandlePerms::SIGNAL
            | HandlePerms::DUPLICATE
            | HandlePerms::TRANSFER)
            .bits();
        slot.entry = Some(HandleEntry {
            generation: slot.generation,
            permissions: perms,
            object,
        });
        Handle::new(slot_idx, slot.generation, perms)
    }

    fn find_free_slot(&mut self) -> u16 {
        for i in 0..self.slots.len() {
            if self.slots[i].entry.is_none() {
                return i as u16;
            }
        }
        let idx = self.slots.len() as u16;
        self.slots.push(HandleSlot { generation: 0, entry: None });
        idx
    }

    /// Look up a handle (mutable), validating generation counter.
    pub fn get_mut(&mut self, handle: Handle) -> Option<&mut KernelObject> {
        let slot = handle.slot() as usize;
        let gen_val = handle.generation();
        self.slots.get_mut(slot)
            .and_then(|s| s.entry.as_mut())
            .filter(|e| e.generation == gen_val)
            .map(|e| &mut e.object)
    }

    /// Look up a handle (immutable), validating generation counter.
    pub fn get(&self, handle: Handle) -> Option<&KernelObject> {
        let slot = handle.slot() as usize;
        let gen_val = handle.generation();
        self.slots.get(slot)
            .and_then(|s| s.entry.as_ref())
            .filter(|e| e.generation == gen_val)
            .map(|e| &e.object)
    }

    /// Remove a handle from the table, returning the object if it existed.
    pub fn remove(&mut self, handle: Handle) -> Option<KernelObject> {
        let slot = handle.slot() as usize;
        let gen_val = handle.generation();
        let slot = self.slots.get_mut(slot)?;
        if let Some(entry) = &slot.entry {
            if entry.generation == gen_val {
                return slot.entry.take().map(|e| e.object);
            }
        }
        None
    }

    /// Check whether the handle has the required permission bits set.
    /// Uses the stored permissions from the handle table (not the raw handle bits).
    pub fn check_perm(&self, handle: Handle, required: HandlePerms) -> bool {
        let slot = handle.slot() as usize;
        let gen_val = handle.generation();
        self.slots.get(slot)
            .and_then(|s| s.entry.as_ref())
            .filter(|e| e.generation == gen_val)
            .map_or(false, |e| (e.permissions & required.bits()) == required.bits())
    }

    /// Iterate over all handle objects mutably (for cleanup / thread exit).
    pub fn iter_objects_mut(
        &mut self,
    ) -> impl Iterator<Item = &mut KernelObject> {
        self.slots
            .iter_mut()
            .filter_map(|slot| slot.entry.as_mut().map(|e| &mut e.object))
    }

    /// Get all handles with their objects.
    pub fn entries(
        &self,
    ) -> impl Iterator<Item = (Handle, &KernelObject)> {
        self.slots.iter().enumerate().filter_map(|(i, slot)| {
            slot.entry.as_ref().map(|e| {
                let h = Handle::new(i as u16, e.generation, e.permissions);
                (h, &e.object)
            })
        })
    }

    /// Get all handles with mutable object references.
    pub fn entries_mut(
        &mut self,
    ) -> impl Iterator<Item = (Handle, &mut KernelObject)> {
        self.slots.iter_mut().enumerate().filter_map(|(i, slot)| {
            slot.entry.as_mut().map(|e| {
                let h = Handle::new(i as u16, e.generation, e.permissions);
                (h, &mut e.object)
            })
        })
    }
}

/// Per-process resources: file descriptors, kernel object handles.
pub struct ProcessResources {
    pub fd_table: spin::Mutex<FdTable>,
    pub handle_table: spin::Mutex<HandleTable>,
}

impl ProcessResources {
    pub fn new() -> Self {
        Self {
            fd_table: spin::Mutex::new(FdTable::new()),
            handle_table: spin::Mutex::new(HandleTable::new()),
        }
    }

    /// Clean up all resources held by this process.
    /// Returns PIDs of waiters that need unblocking (caller must unblock
    /// outside the process-manager lock to avoid deadlock).
    pub fn cleanup(&mut self) -> Vec<ProcessId> {
        let mut to_unblock = Vec::new();

        // Take all handle entries for cleanup.
        let mut ht = self.handle_table.lock();
        let handles: Vec<Handle> = ht.entries().map(|(h, _)| h).collect();
        for handle in handles {
            if let Some(obj) = ht.remove(handle) {
                match obj {
                    KernelObject::Event(e) => {
                        let mut inner = e.inner.lock();
                        to_unblock.append(&mut inner.waiters);
                    }
                    KernelObject::Thread(t) => {
                        let mut inner = t.inner.lock();
                        to_unblock.append(&mut inner.waiters);
                    }
                    KernelObject::Channel(ch) => {
                        let mut inner = ch.inner.lock();
                        to_unblock.append(&mut inner.waiters);
                    }
                    KernelObject::Window(w) => {
                        // Notify compositor that window is gone
                        crate::contexts::kernel::with_kernel_mut(|k| {
                            if let Some(win) = k.window.windows.iter_mut().find(|win| win.id == w.window_id) {
                                win.visible = false;
                            }
                        });
                    }
                    _ => {}
                }
            }
        }
        drop(ht);

        // Clear fd table
        let mut ft = self.fd_table.lock();
        ft.entries.clear();
        drop(ft);

        to_unblock
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
    /// Opaque data for async task futures (used by task.rs spawn/entry)
    pub task_data: u64,
    /// Runtime dispatch mode (Fullerene native, Linux ABI, etc.)
    pub dispatch_mode: Option<DispatchMode>,
    /// Per-process VDSO page for no-interrupt syscalls
    pub vdso_page: Option<VdsoPageRef>,
    /// Per-process resources (fd table, handle table)
    pub resources: ProcessResources,
}

impl Process {
    /// Create a new process
    pub fn new(name: &'static str, entry_point: VirtAddr, is_user: bool) -> Self {
        let id = PROCESS_MANAGER.allocate_pid();

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
            task_data: 0,
            dispatch_mode: None,
            vdso_page: None,
            resources: ProcessResources::new(),
        }
    }

    /// Initialize process context for first execution
    pub fn init_context(&mut self, kernel_stack_top: VirtAddr) {
        petroleum::mem_debug!("Process: init_context for ");
        petroleum::mem_debug!(self.name);
        petroleum::mem_debug!("\n");

        if self.is_user {
            // For user processes, the context RSP should be the user stack
            self.context.regs[7] = self.user_stack.as_u64(); // rsp
            self.context.segments[0] = crate::gdt::user_code()
                .as_ref()
                .map_or(1, |s| s.0 as u64); // cs
            self.context.segments[1] = crate::gdt::user_data()
                .as_ref()
                .map_or(2, |s| s.0 as u64); // ss
        } else {
            // For kernel processes, the context RSP is the kernel stack
            self.context.regs[7] = kernel_stack_top.as_u64(); // rsp
            self.context.segments[0] = crate::gdt::code()
                .as_ref()
                .map(|s| s.0 as u64)
                .unwrap_or(1); // cs
            self.context.segments[1] = crate::gdt::kernel_data()
                .as_ref()
                .map(|s| s.0 as u64)
                .unwrap_or(2); // ss
        }

        // Set RIP to entry point directly
        self.context.rip = self.entry_point.as_u64();
        petroleum::mem_debug!("Process: RIP set, RSP set\n");
        self.context.regs[0] = 0; // rax
        self.context.rflags = 0x202; // Set Interrupt Enable flag
    }
}

/// Manages the global list of processes with encapsulated locking
///
/// Also owns the scheduler state (`next_pid`, `current_index`, `current_pid`)
/// that was previously scattered as separate statics.
pub struct ProcessManager {
    processes: Mutex<HeaplessVec<(ProcessId, Box<Process>), MAX_PROCESSES>>,
    /// Next available process ID.
    next_pid: AtomicUsize,
    /// Round‑robin index into `processes`.
    current_index: AtomicUsize,
    /// PID of the currently running process (0 = none).
    current_pid: AtomicUsize,
}

impl ProcessManager {
    pub const fn new() -> Self {
        Self {
            processes: Mutex::new(HeaplessVec::new()),
            next_pid: AtomicUsize::new(1),
            current_index: AtomicUsize::new(0),
            current_pid: AtomicUsize::new(0),
        }
    }

    // ── PID allocation ─────────────────────────────────────────

    /// Allocate a new unique process ID.
    pub fn allocate_pid(&self) -> ProcessId {
        ProcessId(self.next_pid.fetch_add(1, Ordering::Relaxed) as u64)
    }

    // ── Scheduler state ────────────────────────────────────────

    /// The PID of the currently running process (0 = none / idle).
    pub fn current_pid(&self) -> usize {
        self.current_pid.load(Ordering::SeqCst)
    }

    /// Set the currently running PID.
    pub fn set_current_pid(&self, pid: usize) {
        self.current_pid.store(pid, Ordering::SeqCst);
    }

    /// Get the round‑robin schedule index.
    pub fn schedule_index(&self) -> usize {
        self.current_index.load(Ordering::SeqCst)
    }

    /// Set the round‑robin schedule index.
    pub fn set_schedule_index(&self, idx: usize) {
        self.current_index.store(idx, Ordering::SeqCst);
    }

    // ── Process list operations ─────────────────────────────────

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
        F: FnOnce(&mut HeaplessVec<(ProcessId, Box<Process>), MAX_PROCESSES>) -> R,
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

// Use KERNEL_STACK_SIZE from crate::heap

/// Static idle context storage (avoid heap allocation during early init)
#[allow(static_mut_refs)]
static mut IDLE_CONTEXT: core::mem::MaybeUninit<ProcessContext> = core::mem::MaybeUninit::uninit();
/// Static idle process storage
#[allow(static_mut_refs)]
static mut IDLE_PROCESS: core::mem::MaybeUninit<Process> = core::mem::MaybeUninit::uninit();

/// Initialize process management system
pub fn init(heap_start: usize, heap_end: usize) {
    mem_debug!("Process: init start\n");

    let mut buf = [0u8; 16];
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [Process::init] Heap range: 0x");
    let len = petroleum::serial::format_hex_to_buffer(heap_start as u64, &mut buf, 16);
    petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b" - 0x");
    let len = petroleum::serial::format_hex_to_buffer(heap_end as u64, &mut buf, 16);
    petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"\n");

    // Build idle process completely from static storage (no heap allocation at all).
    // Process::new calls Box::new(ProcessContext) which would fail, so we
    // construct everything manually using static storage.
    let idle_addr = VirtAddr::new(idle_loop as *const () as usize as u64);
    let pid = PROCESS_MANAGER.allocate_pid();

    unsafe {
        // Initialize static context
        let ctx_ptr = core::ptr::addr_of_mut!(IDLE_CONTEXT).cast::<ProcessContext>();
        ctx_ptr.write(ProcessContext {
            regs: [0; 16],
            rflags: 0x0202,
            rip: idle_addr.as_u64(),
            segments: [
                crate::gdt::code()
                    .as_ref()
                    .map(|s| s.0 as u64)
                    .unwrap_or(1),
                crate::gdt::kernel_data()
                    .as_ref()
                    .map(|s| s.0 as u64)
                    .unwrap_or(2),
                0,
                0,
                0,
                0,
            ],
            tss: 0,
            is_user: false,
        });
        mem_debug!("Process: idle context RIP: 0x");
        let mut buf = [0u8; 16];
        let len = petroleum::serial::format_hex_to_buffer(idle_addr.as_u64(), &mut buf, 16);
        petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
        petroleum::write_serial_bytes(0x3F8, 0x3FD, b"\n");

        // Take ownership of static context via Box::from_raw (no heap allocation)
        let ctx_box: Box<ProcessContext> = Box::from_raw(ctx_ptr);

        // Initialize static process, reusing static storage
        let proc_ptr = core::ptr::addr_of_mut!(IDLE_PROCESS).cast::<Process>();
        proc_ptr.write(Process {
            id: pid,
            name: "idle",
            state: ProcessState::Running,
            context: ctx_box,
            page_table_phys_addr: PhysAddr::new(0),
            page_table: None,
            kernel_stack: VirtAddr::new(0),
            user_stack: VirtAddr::new(0),
            entry_point: idle_addr,
            is_user: false,
            exit_code: None,
            parent_id: None,
            task_data: 0,
            dispatch_mode: None,
            vdso_page: None,
            resources: ProcessResources::new(),
        });

        // Take ownership of static process via Box::from_raw
        let proc_box: Box<Process> = Box::from_raw(proc_ptr);
        PROCESS_MANAGER
            .add(proc_box)
            .expect("Failed to add idle process");
    }
    PROCESS_MANAGER.set_current_pid(pid.0 as usize);

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

        // Create VDSO page after page table creation
        let page_table = match crate::memory_management::create_process_page_table() {
            Ok(pt) => pt,
            Err(e) => {
                log::error!("Failed to create process page table: {:?}", e);
                petroleum::common::memory::deallocate_layout(user_stack_ptr, user_stack_layout);
                petroleum::common::memory::deallocate_layout(stack_ptr, stack_layout);
                return Err(e);
            }
        };
        let page_table_phys = page_table.current_page_table() as u64;
        process.page_table_phys_addr = PhysAddr::new(page_table_phys);
        process.page_table = Some(Box::new(page_table));

        let mut fa_lock = crate::heap::FRAME_ALLOCATOR.lock();
        let fa = fa_lock.as_mut().ok_or_else(|| {
            petroleum::common::memory::deallocate_layout(user_stack_ptr, user_stack_layout);
            petroleum::common::memory::deallocate_layout(stack_ptr, stack_layout);
            if let Some(ref page_table) = process.page_table {
                if let Some(pml4_frame) = page_table.pml4_frame() {
                    crate::memory_management::deallocate_process_page_table(pml4_frame);
                }
            }
            petroleum::common::logging::SystemError::InternalError
        })?;
        let pt: &mut petroleum::page_table::process::ProcessPageTable =
            process.page_table.as_mut().unwrap();
        let vdso_ref = create_vdso_page(pt, fa, process.id.0).map_err(|_| {
            petroleum::common::memory::deallocate_layout(user_stack_ptr, user_stack_layout);
            petroleum::common::memory::deallocate_layout(stack_ptr, stack_layout);
            if let Some(ref page_table) = process.page_table {
                if let Some(pml4_frame) = page_table.pml4_frame() {
                    crate::memory_management::deallocate_process_page_table(pml4_frame);
                }
            }
            petroleum::common::logging::SystemError::FrameAllocationFailed
        })?;
        drop(fa_lock);
        process.vdso_page = Some(vdso_ref);
    } else {
        // Create page table for the process (kernel process, no user stack)
        let page_table = match crate::memory_management::create_process_page_table() {
            Ok(pt) => pt,
            Err(e) => {
                log::error!("Failed to create process page table: {:?}", e);
                petroleum::common::memory::deallocate_layout(stack_ptr, stack_layout);
                return Err(e);
            }
        };
        let page_table_phys = page_table.current_page_table() as u64;
        process.page_table_phys_addr = PhysAddr::new(page_table_phys);
        process.page_table = Some(Box::new(page_table));
    }

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
    let to_unblock = PROCESS_MANAGER.with_process(pid, |process| {
        process.state = ProcessState::Terminated;
        process.exit_code = Some(exit_code);

        // Clean up per-process resources (fd table, handle table)
        // Collects waiters to unblock outside the process-manager lock.
        let waiters = process.resources.cleanup();

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

        waiters
    }).unwrap_or_default();

    // Unblock waiters (handles, parent) outside the process-manager lock.
    for waiter in to_unblock {
        unblock_process(waiter);
    }
    unblock_waiting_parents(pid);

    // If current process is terminating, schedule next
    let current_pid = PROCESS_MANAGER.current_pid();
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

        let current_index = PROCESS_MANAGER.schedule_index();
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

        PROCESS_MANAGER.set_schedule_index(next_index);
        let next_pid = process_list[next_index].1.id.0 as usize;
        PROCESS_MANAGER.set_current_pid(next_pid);
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
    let pid = PROCESS_MANAGER.current_pid();
    if pid == 0 {
        None
    } else {
        Some(ProcessId(pid as u64))
    }
}

/// Yield current process
pub fn yield_current() {
    let old_pid = current_pid().expect("yield_current called with no current process");
    schedule_next();
    let new_pid = current_pid().expect("schedule_next failed to select a process");
    unsafe {
        context_switch(Some(old_pid), new_pid);
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

        if let Some(ptr) = new_ptr {
            // Get pointers to the context field specifically to ensure proper alignment
            let old_ctx =
                old_ptr.map(|p| unsafe { (&mut *p).context.as_mut() as *mut ProcessContext });
            let new_ctx = Some(unsafe { (&*ptr).context.as_ref() as *const ProcessContext });
            let pt = unsafe { (*ptr).page_table_phys_addr };
            (old_ctx, new_ctx, Some(pt))
        } else {
            (None, None, None)
        }
    });

    if let (Some(old_ctx_ptr), Some(new_ctx_ptr), Some(pt)) =
        (old_context_ptr, new_context_ptr, new_page_table)
    {
        unsafe {
            let new_frame = PhysFrame::containing_address(pt);
            let (current_frame, _) = Cr3::read();
            if new_frame != current_frame {
                Cr3::write(new_frame, x86_64::registers::control::Cr3Flags::empty());
            }
            // Convert raw pointers to references
            let old_ctx_ref = &mut *old_ctx_ptr;
            let new_ctx_ref = &*new_ctx_ptr;
            // Pass the old context as Some(&mut ProcessContext)
            switch_context(Some(old_ctx_ref), new_ctx_ref);
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
        // Initialize the process management system with dummy heap range
        init(0, 0);
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
// Test process functions (only available in test builds)
// Called by test infrastructure via entry point references.
#[cfg(test)]
#[allow(dead_code)]
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
