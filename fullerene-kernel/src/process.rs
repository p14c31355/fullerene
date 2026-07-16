//! Process management module for Fullerene OS
//!
//! This module defines the `Process` struct, `ProcessContext`, and
//! lifecycle functions (create / terminate).  Scheduling logic lives
//! in [`scheduler_context`]; access the global scheduler via
//! `SCHEDULER`.

use alloc::boxed::Box;
use alloc::collections::btree_map::BTreeMap;
use alloc::vec::Vec;
use core::alloc::Layout;
use petroleum::mem_debug;
use petroleum::page_table::PageTableHelper as _;
use x86_64::{PhysAddr, VirtAddr};

use crate::linux::runtime::DispatchMode;
use crate::vdso::{VdsoPageRef, create_vdso_page};

use crate::syscall::{Handle, HandlePerms, KernelObject};

/// Maximum number of processes managed by the system
pub const MAX_PROCESSES: usize = 64;

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
                crate::gdt::code().as_ref().map_or(1, |s| s.0 as u64), // cs
                crate::gdt::kernel_data().as_ref().map_or(2, |s| s.0 as u64), // ss
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
    generation: u8,
    permissions: u8,
    object: KernelObject,
}

/// A slot that retains its generation across free/alloc cycles for
/// stale-handle (use-after-free) protection.
struct HandleSlot {
    generation: u8,
    entry: Option<HandleEntry>,
}

/// Per-process handle table using slot-based allocation with generation counters
/// and cryptographically signed handles.
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
        // Handle::new computes a cryptographic MAC over (slot, generation, perms)
        // using the per-boot secret.  Only the kernel can produce a valid handle.
        Handle::new(slot_idx as u8, slot.generation, perms)
    }

    fn find_free_slot(&mut self) -> u16 {
        for i in 0..self.slots.len() {
            if self.slots[i].entry.is_none() {
                return i as u16;
            }
        }
        let idx = self.slots.len() as u16;
        self.slots.push(HandleSlot {
            generation: 0,
            entry: None,
        });
        idx
    }

    /// Validate MAC and look up a handle (mutable).
    /// First checks `handle.is_valid()` to reject forged or corrupted handles,
    /// then verifies the generation counter prevents use-after-free.
    pub fn get_mut(&mut self, handle: Handle) -> Option<&mut KernelObject> {
        if !handle.is_valid() {
            return None;
        }
        let slot = handle.slot() as usize;
        let gen_val = handle.generation();
        self.slots
            .get_mut(slot)
            .and_then(|s| s.entry.as_mut())
            .filter(|e| e.generation == gen_val)
            .map(|e| &mut e.object)
    }

    /// Validate MAC and look up a handle (immutable).
    pub fn get(&self, handle: Handle) -> Option<&KernelObject> {
        if !handle.is_valid() {
            return None;
        }
        let slot = handle.slot() as usize;
        let gen_val = handle.generation();
        self.slots
            .get(slot)
            .and_then(|s| s.entry.as_ref())
            .filter(|e| e.generation == gen_val)
            .map(|e| &e.object)
    }

    /// Remove a handle after MAC validation.
    pub fn remove(&mut self, handle: Handle) -> Option<KernelObject> {
        if !handle.is_valid() {
            return None;
        }
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

    /// Check permissions after MAC validation.
    pub fn check_perm(&self, handle: Handle, required: HandlePerms) -> bool {
        if !handle.is_valid() {
            return false;
        }
        let slot = handle.slot() as usize;
        let gen_val = handle.generation();
        self.slots
            .get(slot)
            .and_then(|s| s.entry.as_ref())
            .filter(|e| e.generation == gen_val)
            .map_or(false, |e| {
                (e.permissions & required.bits()) == required.bits()
            })
    }

    /// Iterate over all handle objects mutably (for cleanup / thread exit).
    pub fn iter_objects_mut(&mut self) -> impl Iterator<Item = &mut KernelObject> {
        self.slots
            .iter_mut()
            .filter_map(|slot| slot.entry.as_mut().map(|e| &mut e.object))
    }

    /// Get all handles with their objects.
    pub fn entries(&self) -> impl Iterator<Item = (Handle, &KernelObject)> {
        self.slots.iter().enumerate().filter_map(|(i, slot)| {
            slot.entry.as_ref().map(|e| {
                let h = Handle::new(i as u8, e.generation, e.permissions);
                (h, &e.object)
            })
        })
    }

    /// Get all handles with mutable object references.
    pub fn entries_mut(&mut self) -> impl Iterator<Item = (Handle, &mut KernelObject)> {
        self.slots.iter_mut().enumerate().filter_map(|(i, slot)| {
            slot.entry.as_mut().map(|e| {
                let h = Handle::new(i as u8, e.generation, e.permissions);
                (h, &mut e.object)
            })
        })
    }
}

/// Per-process resources: file descriptors, kernel object handles, event subscriptions.
pub struct ProcessResources {
    pub fd_table: spin::Mutex<FdTable>,
    pub handle_table: spin::Mutex<HandleTable>,
    /// Registered event subscriptions: (event_type, event_handle)
    pub subscriptions: spin::Mutex<alloc::vec::Vec<(u64, u64)>>,
}

impl ProcessResources {
    pub fn new() -> Self {
        Self {
            fd_table: spin::Mutex::new(FdTable::new()),
            handle_table: spin::Mutex::new(HandleTable::new()),
            subscriptions: spin::Mutex::new(alloc::vec::Vec::new()),
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
                            if let Some(win) = k
                                .window
                                .windows
                                .iter_mut()
                                .find(|win| win.id == w.window_id)
                            {
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
        let id = SCHEDULER.allocate_pid();

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
            self.context.segments[0] = crate::gdt::user_code().as_ref().map_or(1, |s| s.0 as u64); // cs
            self.context.segments[1] = crate::gdt::user_data().as_ref().map_or(2, |s| s.0 as u64); // ss
        } else {
            // For kernel processes, the context RSP is the kernel stack
            self.context.regs[7] = kernel_stack_top.as_u64(); // rsp
            self.context.segments[0] = crate::gdt::code().as_ref().map(|s| s.0 as u64).unwrap_or(1); // cs
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

/// Scheduling and process-list state lives in [`crate::scheduler_context::SCHEDULER`].
///
/// Use the convenience functions below (which delegate to `SCHEDULER`) or
/// access `SCHEDULER` directly.
pub use crate::scheduler_context::SCHEDULER;

// Use KERNEL_STACK_SIZE from crate::heap

/// Marker used to track whether the idle process has been initialised.
static IDLE_INIT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Initialize process management system
pub fn init(heap_start: usize, heap_end: usize) {
    // Check if already initialized
    if IDLE_INIT.load(core::sync::atomic::Ordering::Acquire) {
        return;
    }

    mem_debug!("Process: init start\n");

    let mut buf = [0u8; 16];
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [Process::init] Heap range: 0x");
    let len = petroleum::serial::format_hex_to_buffer(heap_start as u64, &mut buf, 16);
    petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b" - 0x");
    let len = petroleum::serial::format_hex_to_buffer(heap_end as u64, &mut buf, 16);
    petroleum::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
    petroleum::write_serial_bytes(0x3F8, 0x3FD, b"\n");

    // Idle process — heap allocator is already initialised (see init.rs),
    // so we can safely use Box::new here.
    let idle_addr = VirtAddr::new(idle_loop as *const () as usize as u64);
    let pid = SCHEDULER.allocate_pid();

    let ctx = ProcessContext {
        regs: [0; 16],
        rflags: 0x0202,
        rip: idle_addr.as_u64(),
        segments: [
            crate::gdt::code().as_ref().map(|s| s.0 as u64).unwrap_or(1),
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
    };

    let idle = Box::new(Process {
        id: pid,
        name: "idle",
        state: ProcessState::Running,
        context: Box::new(ctx),
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

    SCHEDULER.add(idle).expect("Failed to add idle process");

    IDLE_INIT.store(true, core::sync::atomic::Ordering::Release);
    SCHEDULER.set_current_pid(pid.0 as usize);

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
                unsafe { petroleum::common::memory::deallocate_layout(stack_ptr, stack_layout) };
                e
            })?;
        process.user_stack =
            VirtAddr::new(user_stack_ptr as u64 + crate::heap::KERNEL_STACK_SIZE as u64);

        // Create VDSO page after page table creation
        let page_table = match crate::memory_management::create_process_page_table() {
            Ok(pt) => pt,
            Err(e) => {
                log::error!("Failed to create process page table: {:?}", e);
                unsafe {
                    petroleum::common::memory::deallocate_layout(user_stack_ptr, user_stack_layout);
                    petroleum::common::memory::deallocate_layout(stack_ptr, stack_layout);
                }
                return Err(e);
            }
        };
        let page_table_phys = page_table.current_page_table() as u64;
        process.page_table_phys_addr = PhysAddr::new(page_table_phys);
        process.page_table = Some(Box::new(page_table));

        let mut fa_lock = crate::heap::FRAME_ALLOCATOR.lock();
        let fa = fa_lock.as_mut().ok_or_else(|| {
            unsafe {
                petroleum::common::memory::deallocate_layout(user_stack_ptr, user_stack_layout);
                petroleum::common::memory::deallocate_layout(stack_ptr, stack_layout);
            }
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
            unsafe {
                petroleum::common::memory::deallocate_layout(user_stack_ptr, user_stack_layout);
                petroleum::common::memory::deallocate_layout(stack_ptr, stack_layout);
            }
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
                unsafe { petroleum::common::memory::deallocate_layout(stack_ptr, stack_layout) };
                return Err(e);
            }
        };
        let page_table_phys = page_table.current_page_table() as u64;
        process.page_table_phys_addr = PhysAddr::new(page_table_phys);
        process.page_table = Some(Box::new(page_table));
    }

    process.init_context(kernel_stack_top);

    let pid = process.id;
    SCHEDULER.add(Box::new(process))?;

    mem_debug!("Process: create_process done\n");
    Ok(pid)
}

/// Unblock parent processes that are waiting for this child process
fn unblock_waiting_parents(child_pid: ProcessId) {
    let parent_to_unblock = SCHEDULER.with_list(|list| {
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
    let to_unblock = SCHEDULER
        .with_process(pid, |process| {
            // The idle task owns neither an allocated stack nor a replacement task.
            // It is a scheduler invariant, not a terminable user process.
            if process.name == "idle" {
                return Vec::new();
            }
            process.state = ProcessState::Terminated;
            process.exit_code = Some(exit_code);

            // Clean up per-process resources (fd table, handle table)
            // Collects waiters to unblock outside the process-manager lock.
            let waiters = process.resources.cleanup();

            // Free resources
            if let Some(kernel_stack_base) = process
                .kernel_stack
                .as_u64()
                .checked_sub(crate::heap::KERNEL_STACK_SIZE as u64)
                .filter(|&base| base != 0)
            {
                let layout = Layout::from_size_align(crate::heap::KERNEL_STACK_SIZE, 16).unwrap();
                unsafe {
                    petroleum::common::memory::deallocate_layout(
                        kernel_stack_base as *mut u8,
                        layout,
                    )
                };
            }

            // Properly free page table frames recursively
            if let Some(page_table) = process.page_table.take() {
                if let Some(pml4_frame) = page_table.pml4_frame() {
                    drop(page_table);
                    crate::memory_management::deallocate_process_page_table(pml4_frame);
                }
            }

            process.page_table = None;

            waiters
        })
        .unwrap_or_default();

    // Unblock waiters (handles, parent) outside the process-manager lock.
    for waiter in to_unblock {
        unblock_process(waiter);
    }
    unblock_waiting_parents(pid);

    // If current process is terminating, schedule next
    let current_pid = SCHEDULER.current_pid();
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
    SCHEDULER.schedule_next();
}

/// Get current process ID
pub fn current_pid() -> Option<ProcessId> {
    let pid = SCHEDULER.current_pid();
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
    unsafe { SCHEDULER.context_switch(old_pid, new_pid) };
}

/// Block current process
pub fn block_current() {
    SCHEDULER.block_current();
}

/// Unblock a process
pub fn unblock_process(pid: ProcessId) {
    SCHEDULER.with_process(pid, |process| {
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
        assert!(SCHEDULER.count() > 0);
        assert!(SCHEDULER.active_count() > 0);
    }
}

#[cfg(test)]
pub fn test_process_main() {
    let message = b"Hello from test user process!\n";
    petroleum::write(1, message);
    let pid = petroleum::getpid();
    petroleum::write(1, b"My PID is: ");
    let pid_str = alloc::format!("{}\n", pid);
    petroleum::write(1, pid_str.as_bytes());
    petroleum::sleep();
    petroleum::sleep();
    petroleum::exit(0);
}
