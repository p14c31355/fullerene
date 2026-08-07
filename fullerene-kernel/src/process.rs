//! Process management module for Fullerene OS
//!
//! This module defines the `Process` struct, `ProcessContext`, and
//! lifecycle functions (create / terminate).  Scheduling logic lives
//! in [`scheduler_context`]; access the global scheduler via
//! `SCHEDULER`.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::sync::atomic::{AtomicBool, Ordering};
use petroleum::mem_debug;
use petroleum::page_table::{FrameAllocatorExt, PageTableHelper as _};
use x86_64::structures::paging::{FrameAllocator as _, PageTableFlags};
use x86_64::{PhysAddr, VirtAddr};

use crate::solvent_linux::runtime::DispatchMode;
use crate::vdso::{VdsoPageRef, create_vdso_page};

use crate::syscall::{Handle, HandlePerms, KernelObject};

/// Maximum number of processes managed by the system
pub const MAX_PROCESSES: usize = 64;

const NATIVE_USER_STACK_TOP: u64 = 0x0000_7fff_fffe_f000;
const NATIVE_USER_STACK_SIZE: usize = 64 * 1024;

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

/// Register snapshot kept when a user process is stopped by a CPU fault.
///
/// This is the kernel-side "last safe footprint": the faulting instruction
/// is never resumed, while the scheduler can still report exactly where the
/// process crossed the boundary before its resources are reaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultRecord {
    pub reason: &'static str,
    pub rip: u64,
    pub rsp: u64,
    pub address: u64,
    pub error_code: u64,
}

/// Named general-purpose register image used for a process's initial entry.
///
/// Suspended kernel continuations are kept on their kernel stack instead of
/// rewriting this image on every context switch.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct GeneralRegisters {
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
}

/// Segment-selector image used when entering a process for the first time.
///
/// `cs` and `ss` are authoritative and are restored by `iretq`. The legacy
/// `ds`, `es`, `fs`, and `gs` selector fields are informational only; long-mode
/// FS/GS bases are managed separately and the context trampoline does not
/// restore these four selectors.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SegmentRegisters {
    pub(crate) cs: u64,
    pub(crate) ss: u64,
    pub(crate) ds: u64,
    pub(crate) es: u64,
    pub(crate) fs: u64,
    pub(crate) gs: u64,
}

/// Process context for context switching.
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
pub struct ProcessContext {
    /// Register image for the first kernel/user entry.
    pub(crate) registers: GeneralRegisters,
    /// CPU flags
    pub(crate) rflags: u64,
    /// Instruction pointer
    pub(crate) rip: u64,
    /// Segment registers: cs, ss, ds, es, fs, gs
    pub(crate) segments: SegmentRegisters,
    /// Stack image created by the low-level switch trampoline. A zero value
    /// means this process has not yet suspended a kernel continuation.
    pub(crate) kernel_rsp: u64,
    /// Whether the process runs in user mode (Ring 3)
    pub(crate) is_user: bool,
}

static_assertions::const_assert_eq!(core::mem::offset_of!(ProcessContext, registers), 0);
static_assertions::const_assert_eq!(core::mem::offset_of!(ProcessContext, rflags), 128);
static_assertions::const_assert_eq!(core::mem::offset_of!(ProcessContext, rip), 136);
static_assertions::const_assert_eq!(core::mem::offset_of!(ProcessContext, segments), 144);
static_assertions::const_assert_eq!(core::mem::offset_of!(ProcessContext, kernel_rsp), 192);
static_assertions::const_assert_eq!(core::mem::offset_of!(ProcessContext, is_user), 200);

impl Default for ProcessContext {
    fn default() -> Self {
        Self {
            registers: GeneralRegisters::default(),
            rflags: 0x0202, // IF flag set
            rip: 0,
            segments: SegmentRegisters {
                // Use fallback segment selectors if GDT is not ready.
                cs: crate::gdt::code().as_ref().map_or(1, |s| s.0 as u64),
                ss: crate::gdt::kernel_data().as_ref().map_or(2, |s| s.0 as u64),
                ds: 0,
                es: 0,
                fs: 0,
                gs: 0,
            },
            kernel_rsp: 0,
            is_user: false,
        }
    }
}

/// Per-process file descriptor table.
pub struct FdSlotMap {
    slots: Vec<Option<crate::fs::FileDesc>>,
}

impl FdSlotMap {
    fn new() -> Self {
        let mut slots = Vec::new();
        slots.resize_with(3, || None);
        Self { slots }
    }

    pub fn insert(&mut self, fd: u32, value: crate::fs::FileDesc) -> Option<crate::fs::FileDesc> {
        let index = fd as usize;
        if self.slots.len() <= index {
            self.slots.resize_with(index + 1, || None);
        }
        self.slots[index].replace(value)
    }

    pub fn get_mut(&mut self, fd: &u32) -> Option<&mut crate::fs::FileDesc> {
        self.slots.get_mut(*fd as usize)?.as_mut()
    }

    pub fn remove(&mut self, fd: &u32) -> Option<crate::fs::FileDesc> {
        self.slots.get_mut(*fd as usize)?.take()
    }

    pub fn contains_key(&self, fd: &u32) -> bool {
        self.slots.get(*fd as usize).is_some_and(Option::is_some)
    }

    pub fn clear(&mut self) {
        for slot in self.slots.iter_mut().skip(3) {
            *slot = None;
        }
    }

    fn first_free_from(&self, start: u32) -> Option<u32> {
        self.slots
            .iter()
            .enumerate()
            .skip(start as usize)
            .find_map(|(index, slot)| slot.is_none().then_some(index as u32))
    }
}

pub struct FdTable {
    pub entries: FdSlotMap,
}

impl FdTable {
    pub fn new() -> Self {
        Self {
            entries: FdSlotMap::new(),
        }
    }

    pub fn alloc(&mut self, file_desc: crate::fs::FileDesc) -> Result<u32, ()> {
        let fd = self
            .entries
            .first_free_from(3)
            .map(Ok)
            .unwrap_or_else(|| u32::try_from(self.entries.slots.len()).map_err(|_| ()))?;
        self.entries.insert(fd, file_desc);
        Ok(fd)
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
}

impl HandleTable {
    pub fn new() -> Self {
        Self {
            slots: alloc::vec::Vec::new(),
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
    /// Owned process name; callers may provide a transient label.
    pub name: Box<str>,
    /// Current state
    pub state: ProcessState,
    /// CPU context for context switching
    pub context: Box<ProcessContext>,
    /// Per-process x87/SSE/AVX state image used by XSAVE/XRSTOR.
    pub(crate) fpu_state: Box<crate::fpu::XsaveState>,
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
    /// CPU fault that caused termination, if any.
    pub fault: Option<FaultRecord>,
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
    pub fn new(name: &str, entry_point: VirtAddr, is_user: bool) -> Self {
        let id = SCHEDULER.allocate_pid();

        Self {
            id,
            name: Box::from(name),
            state: ProcessState::Ready,
            context: Box::new(ProcessContext::default()),
            fpu_state: Box::new(crate::fpu::XsaveState::initial()),
            page_table_phys_addr: PhysAddr::new(0), // Will be set when allocated
            page_table: None,
            kernel_stack: VirtAddr::new(0), // Will be set when allocated
            user_stack: VirtAddr::new(0),   // Will be set when allocated
            entry_point,
            is_user,
            exit_code: None,
            fault: None,
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
        petroleum::mem_debug!(self.name.as_ref());
        petroleum::mem_debug!("\n");

        self.context.kernel_rsp = 0;
        self.context.is_user = self.is_user;
        if self.is_user {
            // For user processes, the context RSP should be the user stack
            self.context.registers.rsp = self.user_stack.as_u64();
            self.context.segments.cs = crate::gdt::user_code().as_ref().map_or(1, |s| s.0 as u64);
            self.context.segments.ss = crate::gdt::user_data().as_ref().map_or(2, |s| s.0 as u64);
        } else {
            // For kernel processes, the context RSP is the kernel stack
            self.context.registers.rsp = kernel_stack_top.as_u64();
            self.context.segments.cs = crate::gdt::code().as_ref().map(|s| s.0 as u64).unwrap_or(1);
            self.context.segments.ss = crate::gdt::kernel_data()
                .as_ref()
                .map(|s| s.0 as u64)
                .unwrap_or(2);
        }

        // Set RIP to entry point directly
        self.context.rip = self.entry_point.as_u64();
        petroleum::mem_debug!("Process: RIP set, RSP set\n");
        self.context.registers.rax = 0;
        self.context.rflags = 0x202; // Set Interrupt Enable flag
    }
}

/// Scheduling and process-list state lives in [`crate::scheduler_context::SCHEDULER`].
///
/// Use the convenience functions below (which delegate to `SCHEDULER`) or
/// access `SCHEDULER` directly.
pub use crate::scheduler_context::SCHEDULER;

/// PID requested by a shell command that just created a process.  The actual
/// switch is deferred until terminal control has returned to the polling
/// loop, so no shell/VFS/runtime lock is held across the assembly switch.
static PENDING_YIELD_TO: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static HANDOFF_FAILURE_LOGGED: AtomicBool = AtomicBool::new(false);

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
        registers: GeneralRegisters::default(),
        rflags: 0x0202,
        rip: idle_addr.as_u64(),
        segments: SegmentRegisters {
            cs: crate::gdt::code().as_ref().map(|s| s.0 as u64).unwrap_or(1),
            ss: crate::gdt::kernel_data()
                .as_ref()
                .map(|s| s.0 as u64)
                .unwrap_or(2),
            ds: 0,
            es: 0,
            fs: 0,
            gs: 0,
        },
        kernel_rsp: 0,
        is_user: false,
    };

    let idle = Box::new(Process {
        id: pid,
        name: Box::from("idle"),
        state: ProcessState::Running,
        context: Box::new(ctx),
        fpu_state: Box::new(crate::fpu::XsaveState::initial()),
        page_table_phys_addr: PhysAddr::new(0),
        page_table: None,
        kernel_stack: VirtAddr::new(0),
        user_stack: VirtAddr::new(0),
        entry_point: idle_addr,
        is_user: false,
        exit_code: None,
        fault: None,
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
fn map_native_user_stack(
    process: &mut Process,
) -> Result<(), petroleum::common::logging::SystemError> {
    let stack_base = (NATIVE_USER_STACK_TOP as usize)
        .checked_sub(NATIVE_USER_STACK_SIZE)
        .ok_or(petroleum::common::logging::SystemError::InvalidArgument)?;
    let page_count = NATIVE_USER_STACK_SIZE / 4096;
    let page_table = process
        .page_table
        .as_mut()
        .ok_or(petroleum::common::logging::SystemError::NoSuchProcess)?;

    let result = petroleum::page_table::constants::with_frame_allocator(|frame_allocator| {
        let mut mapped_pages = 0usize;
        for index in 0..page_count {
            let virtual_address = stack_base + index * 4096;
            let frame = frame_allocator
                .allocate_frame()
                .ok_or(petroleum::common::logging::SystemError::FrameAllocationFailed)?;
            let physical_address = frame.start_address().as_u64() as usize;
            let flags = PageTableFlags::PRESENT
                | PageTableFlags::WRITABLE
                | PageTableFlags::USER_ACCESSIBLE
                | PageTableFlags::NO_EXECUTE;
            if let Err(error) =
                page_table.map_page(virtual_address, physical_address, flags, frame_allocator)
            {
                frame_allocator.deallocate_frame(petroleum::page_table::types::PhysFrame {
                    start_address: physical_address as u64,
                });
                for rollback in 0..mapped_pages {
                    if let Ok(mapped_frame) = page_table.unmap_page(stack_base + rollback * 4096) {
                        frame_allocator.deallocate_frame(petroleum::page_table::types::PhysFrame {
                            start_address: mapped_frame.start_address().as_u64(),
                        });
                    }
                }
                return Err(error);
            }
            mapped_pages += 1;
        }
        Ok(())
    });
    result.map_err(|error| match error {
        petroleum::common::logging::SystemError::FrameAllocationFailed => error,
        _ => petroleum::common::logging::SystemError::MappingFailed,
    })?;
    // Enter with the SysV-compatible 8-byte offset expected at a function
    // entry (as if a return address had already been pushed). The mapping
    // itself still ends on the page boundary above.
    process.user_stack = VirtAddr::new(NATIVE_USER_STACK_TOP - 8);
    Ok(())
}

pub fn create_process(
    name: &str,
    entry_point_address: VirtAddr,
    is_user: bool,
) -> Result<ProcessId, petroleum::common::logging::SystemError> {
    mem_debug!("Process: create_process starting\n");

    let mut process = Process::new(name, entry_point_address, is_user);

    // Allocate kernel stack for the process
    let stack_layout = Layout::from_size_align(crate::heap::KERNEL_STACK_SIZE, 16).unwrap();
    let stack_ptr = petroleum::common::memory::allocate_layout(stack_layout)?;
    let kernel_stack_top = VirtAddr::new(stack_ptr as u64 + crate::heap::KERNEL_STACK_SIZE as u64);
    process.kernel_stack = kernel_stack_top;

    if is_user {
        // Create VDSO page after page table creation
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

        if let Err(error) = map_native_user_stack(&mut process) {
            unsafe { petroleum::common::memory::deallocate_layout(stack_ptr, stack_layout) };
            if let Some(page_table) = process.page_table.take()
                && let Some(pml4_frame) = page_table.pml4_frame()
            {
                crate::memory_management::deallocate_process_page_table(pml4_frame);
            }
            return Err(error);
        }

        let vdso_result = {
            let pt: &mut petroleum::page_table::process::ProcessPageTable =
                process.page_table.as_mut().unwrap();
            petroleum::page_table::constants::with_frame_allocator(|frame_allocator| {
                create_vdso_page(pt, frame_allocator, process.id.0).map_err(|_| ())
            })
        };
        let vdso_ref = match vdso_result {
            Ok(vdso) => vdso,
            Err(_) => {
                unsafe {
                    petroleum::common::memory::deallocate_layout(stack_ptr, stack_layout);
                }
                if let Some(ref page_table) = process.page_table
                    && let Some(pml4_frame) = page_table.pml4_frame()
                {
                    crate::memory_management::deallocate_process_page_table(pml4_frame);
                }
                return Err(petroleum::common::logging::SystemError::FrameAllocationFailed);
            }
        };
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
    let is_idle = SCHEDULER
        .with_process(pid, |process| process.name.as_ref() == "idle")
        .unwrap_or(false);
    let is_current = SCHEDULER.current_pid() == pid.0 as usize;
    let to_unblock = SCHEDULER
        .with_process(pid, |process| {
            // The idle task owns neither an allocated stack nor a replacement task.
            // It is a scheduler invariant, not a terminable user process.
            if process.name.as_ref() == "idle" {
                return Vec::new();
            }
            process.state = ProcessState::Terminated;
            process.exit_code = Some(exit_code);

            // Clean up per-process resources (fd table, handle table)
            // Collects waiters to unblock outside the process-manager lock.
            let waiters = process.resources.cleanup();

            // An executing process cannot release its own address space before
            // the context switch. The scheduler will reclaim it after the
            // process is no longer current.
            if !is_current {
                // Free resources
                if let Some(kernel_stack_base) = process
                    .kernel_stack
                    .as_u64()
                    .checked_sub(crate::heap::KERNEL_STACK_SIZE as u64)
                    .filter(|&base| base != 0)
                {
                    let layout =
                        Layout::from_size_align(crate::heap::KERNEL_STACK_SIZE, 16).unwrap();
                    unsafe {
                        petroleum::common::memory::deallocate_layout(
                            kernel_stack_base as *mut u8,
                            layout,
                        )
                    };
                    process.kernel_stack = VirtAddr::new(0);
                }

                // Properly free page table frames recursively
                if let Some(page_table) = process.page_table.take() {
                    if let Some(pml4_frame) = page_table.pml4_frame() {
                        drop(page_table);
                        crate::memory_management::deallocate_process_page_table(pml4_frame);
                    }
                }

                process.page_table = None;
            }

            waiters
        })
        .unwrap_or_default();

    // Unblock waiters (handles, parent) outside the process-manager lock.
    for waiter in to_unblock {
        unblock_process(waiter);
    }
    unblock_waiting_parents(pid);

    // If the current process is terminating, switch away before returning.
    // `schedule_next` only changes scheduler state; it does not perform the
    // context switch itself.
    if is_current && !is_idle {
        crate::klog_fmt!(
            "[LINUX-DIAG] exit scheduling pid={} current_cr3={:#x}\n",
            pid.0,
            x86_64::registers::control::Cr3::read()
                .0
                .start_address()
                .as_u64()
        );
        let (old, next) = SCHEDULER.schedule_next();
        crate::klog_fmt!(
            "[LINUX-DIAG] exit scheduled pid={} old={:?} next={} next_state={:?}\n",
            pid.0,
            old.map(|value| value.0),
            next.0,
            SCHEDULER.with_process(next, |process| process.state)
        );
        #[cfg(linux_musl_smoke)]
        if let Some((rip, rsp, kernel_rsp, is_user, state)) =
            SCHEDULER.with_process(next, |process| {
                (
                    process.context.rip,
                    process.context.registers.rsp,
                    process.context.kernel_rsp,
                    process.context.is_user,
                    process.state,
                )
            })
        {
            petroleum::serial::serial_log(format_args!(
                "[linux-smoke] resume PID {} entry_rip={:#x} entry_rsp={:#x} kernel_rsp={:#x} user={} state={:?}\n",
                next.0, rip, rsp, kernel_rsp, is_user, state
            ));
        }
        if old == Some(pid) && next != pid {
            crate::klog_fmt!(
                "[LINUX-DIAG] exit context switch enter old={} next={}\n",
                pid.0,
                next.0
            );
            unsafe { SCHEDULER.context_switch(Some(pid), next) };
        }
        crate::klog_fmt!(
            "[LINUX-DIAG] exit context switch returned pid={} next={}\n",
            pid.0,
            next.0
        );
        petroleum::halt_loop();
    }
}

/// Stop a user process at a CPU exception boundary and retain its fault
/// footprint until the normal scheduler cleanup reaps it.
pub fn mark_faulted(pid: ProcessId, record: FaultRecord) {
    let waiters = SCHEDULER.with_process(pid, |process| {
        process.state = ProcessState::Terminated;
        process.exit_code = Some(128);
        process.fault = Some(record);
        process.resources.cleanup()
    });
    for waiter in waiters.unwrap_or_default() {
        unblock_process(waiter);
    }
    unblock_waiting_parents(pid);
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

/// Append a scheduler milestone to a Linux process's GUI terminal when it
/// has one. This is intentionally best-effort: scheduler diagnostics must
/// never make a non-GUI process or an exception path depend on the desktop.
pub fn mark_linux_stage(pid: ProcessId, stage: &str) {
    let window = SCHEDULER
        .with_process(pid, |process| match process.dispatch_mode.as_ref() {
            Some(crate::solvent_linux::DispatchMode::Linux(runtime)) => runtime.terminal_window,
            _ => None,
        })
        .flatten();
    if let Some(window) = window {
        crate::klog_fmt!(
            "[BUSYBOX-DIAG] scheduler stage={} pid={} window_id={}\n",
            stage,
            pid.0,
            window.0
        );
        solvent::request_frame();
        solvent::mark_klog_live_dirty();
        // The next operation may enter a non-returning context switch, so do
        // not wait for the ordinary event loop to paint this milestone.
        solvent::flush_frame_no_fb();
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

/// Yield from the interactive shell's scheduler handoff callback.
///
/// The callback is used both for the first launch handoff and while Nozzle is
/// waiting for another command. Once a Linux process blocks in `read(0)`, the
/// shell must continue round-robin scheduling it; otherwise keyboard input can
/// be queued forever without the process getting another timeslice.
pub fn yield_from_scheduler_stack() {
    let target = PENDING_YIELD_TO.swap(0, core::sync::atomic::Ordering::AcqRel);
    if target == 0 {
        if SCHEDULER.active_count() > 1 && current_pid().is_some() {
            yield_current();
        }
        return;
    }
    let new_pid = ProcessId(target);
    crate::klog_fmt!(
        "[LINUX-DIAG] scheduler handoff enter current={} target={}\n",
        SCHEDULER.current_pid(),
        target
    );
    if !SCHEDULER.yield_to(new_pid) {
        // The shell may observe the newly-created task during the short
        // interval in which another scheduler path has not yet marked it
        // Ready. Do not lose the explicit handoff in that race: the next
        // terminal poll will retry it instead of leaving the shell waiting
        // forever after a second BusyBox launch.
        let _ = PENDING_YIELD_TO.compare_exchange(0, target, Ordering::AcqRel, Ordering::Acquire);
        if !HANDOFF_FAILURE_LOGGED.swap(true, Ordering::AcqRel) {
            petroleum::serial::serial_log(format_args!(
                "[LINUX-DIAG] scheduler handoff retry current={} target={} state={:?}\n",
                SCHEDULER.current_pid(),
                target,
                SCHEDULER.process_state(new_pid),
            ));
        }
    } else {
        crate::klog_fmt!(
            "[LINUX-DIAG] scheduler handoff resumed current={} target={}\n",
            SCHEDULER.current_pid(),
            target
        );
    }
}

/// Defer a direct handoff until the shell's current command callback returns.
pub fn defer_yield_to(pid: ProcessId) {
    HANDOFF_FAILURE_LOGGED.store(false, Ordering::Release);
    PENDING_YIELD_TO.store(pid.0, core::sync::atomic::Ordering::Release);
}

/// Cooperatively switch directly to a specific ready process.
pub fn yield_to(pid: ProcessId) -> bool {
    SCHEDULER.yield_to(pid)
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

    struct FakeProcessAddressSpace {
        bytes: Vec<u8>,
        writable: Vec<bool>,
    }

    impl FakeProcessAddressSpace {
        fn new(size: usize) -> Self {
            Self {
                bytes: alloc::vec![0; size],
                writable: alloc::vec![false; size],
            }
        }

        fn map_writable(&mut self, start: usize, length: usize) {
            for writable in &mut self.writable[start..start + length] {
                *writable = true;
            }
        }

        fn copy_to_user(&mut self, start: usize, data: &[u8]) -> Result<(), ()> {
            let end = start.checked_add(data.len()).ok_or(())?;
            let destination = self.bytes.get_mut(start..end).ok_or(())?;
            if !self.writable.get(start..end).ok_or(())?.iter().all(|v| *v) {
                return Err(());
            }
            destination.copy_from_slice(data);
            Ok(())
        }
    }

    #[test]
    fn test_process_creation() {
        let addr = VirtAddr::new(0);
        let proc = Process::new("test", addr, false);
        assert_eq!(proc.name.as_ref(), "test");
        assert_eq!(proc.state, ProcessState::Ready);
    }

    #[test]
    fn test_process_counting() {
        // Initialize the process management system with dummy heap range
        init(0, 0);
        assert!(SCHEDULER.count() > 0);
        assert!(SCHEDULER.active_count() > 0);
    }

    #[test]
    fn two_process_resource_tables_are_isolated() {
        let first = ProcessResources::new();
        let second = ProcessResources::new();

        first.fd_table.lock().entries.insert(
            3,
            crate::fs::FileDesc {
                fd: 3,
                ino: 11,
                offset: 7,
                flags: 0,
            },
        );
        let first_handle =
            first
                .handle_table
                .lock()
                .alloc(KernelObject::Device(crate::syscall::DeviceState {
                    pci: Some(nitrogen::pci::PciDevice {
                        bus: 0,
                        device: 0,
                        function: 0,
                        handle: 0,
                        vendor_id: 0,
                        device_id: 0,
                        class_code: 0,
                        subclass: 0,
                        prog_if: 0,
                        header_type: 0,
                    }),
                    name: None,
                }));

        assert!(first.fd_table.lock().entries.contains_key(&3));
        assert!(!second.fd_table.lock().entries.contains_key(&3));
        assert!(first.handle_table.lock().get(first_handle).is_some());
        assert!(second.handle_table.lock().get(first_handle).is_none());
    }

    #[test]
    fn fd_slots_reuse_holes_without_overwriting_later_entries() {
        fn file_desc(ino: u64) -> crate::fs::FileDesc {
            crate::fs::FileDesc {
                fd: 0,
                ino,
                offset: 0,
                flags: 0,
            }
        }

        let mut table = FdTable::new();
        assert_eq!(table.alloc(file_desc(30)), Ok(3));
        assert_eq!(table.alloc(file_desc(40)), Ok(4));
        assert_eq!(table.entries.remove(&3).map(|entry| entry.ino), Some(30));
        assert_eq!(table.alloc(file_desc(31)), Ok(3));
        assert_eq!(table.alloc(file_desc(50)), Ok(5));
        assert_eq!(table.entries.get_mut(&4).map(|entry| entry.ino), Some(40));
    }

    #[test]
    fn fake_process_address_space_rejects_unmapped_user_copy() {
        let mut address_space = FakeProcessAddressSpace::new(32);
        address_space.map_writable(8, 8);
        assert_eq!(address_space.copy_to_user(8, b"full"), Ok(()));
        assert_eq!(&address_space.bytes[8..12], b"full");
        assert_eq!(address_space.copy_to_user(14, b"overflow"), Err(()));
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
