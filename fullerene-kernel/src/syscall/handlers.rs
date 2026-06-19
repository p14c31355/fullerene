use super::interface::{copy_user_string, SyscallError, SyscallResult};
use crate::process;
use crate::process::{Process, ProcessState};
use alloc::boxed::Box;
use alloc::collections::btree_map::BTreeMap;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::sync::atomic::{AtomicU64, Ordering};

use petroleum::common::memory::{user_slice, user_slice_mut};
use spin::Mutex;
use x86_64::{PhysAddr, VirtAddr};

use crate::linux::Runtime as LinuxRuntimeTrait;
use crate::contexts::kernel;

// ── Global tables ──────────────────────────────────────────────

/// Global file-descriptor table mapping integer FDs to FileDesc.
static FD_TABLE: Mutex<BTreeMap<u32, crate::fs::FileDesc>> = Mutex::new(BTreeMap::new());
static NEXT_FD: Mutex<u32> = Mutex::new(3); // Start after stdin/stdout/stderr

/// Global kernel-object handle table.
///
/// Every kernel object (event, thread, window, device, channel, pipe, timer)
/// that is exposed to user-space gets a unique [`Handle`] allocated here.
/// Handles are never re-used within a boot cycle.
static HANDLE_TABLE: Mutex<BTreeMap<u64, KernelObject>> = Mutex::new(BTreeMap::new());
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1000);

/// Opaque kernel-object handle exposed to user-space.
pub type Handle = u64;

/// Set of kernel objects that can be referenced by a [`Handle`].
enum KernelObject {
    Event(EventState),
    Thread(ThreadState),
    Window(WindowState),
    Device(DeviceState),
    Channel(ChannelState),
    Pipe(PipeState),
    Timer(TimerState),
}

// ── Per-object state types ─────────────────────────────────────

struct EventState {
    signaled: bool,
    manual_reset: bool,
    /// PIDs blocked on this event.
    waiters: Vec<process::ProcessId>,
}

struct ThreadState {
    tid: u64,
    pid: process::ProcessId,
    detached: bool,
    exit_code: Option<i32>,
    waiters: Vec<process::ProcessId>,
}

struct WindowState {
    /// Native window index in WindowContext.
    window_index: usize,
    pid: process::ProcessId,
}

struct DeviceState {
    /// Device identifier string.
    device_id: alloc::string::String,
    /// Device class (e.g. "pci", "usb", "input").
    device_class: alloc::string::String,
}

struct ChannelState {
    /// Buffered messages (each message is a Vec<u8>).
    messages: Vec<Vec<u8>>,
    /// PIDs blocked on recv.
    waiters: Vec<process::ProcessId>,
    max_messages: usize,
}

struct PipeState {
    /// Pipe buffer.
    buffer: Vec<u8>,
    /// Whether this is the read end or write end.
    is_read_end: bool,
    /// PID blocked on the other end.
    peer_waiters: Vec<process::ProcessId>,
}

struct TimerState {
    /// Target deadline in uptime microseconds.
    deadline_us: u64,
    /// Optional event handle to signal when the timer fires.
    signal_event: Option<Handle>,
    fired: bool,
}

// ── POSIX-style open flags ─────────────────────────────────────

const O_RDONLY: i32 = 0;
const O_WRONLY: i32 = 1;
const O_RDWR: i32 = 2;
const O_CREAT: i32 = 0x40;
const O_TRUNC: i32 = 0x200;
const O_APPEND: i32 = 0x400;

const KERNEL_STACK_SIZE: usize = 4096;

// ── Handle helper ──────────────────────────────────────────────

fn alloc_handle(obj: KernelObject) -> Handle {
    let h = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    HANDLE_TABLE.lock().insert(h, obj);
    h
}

fn with_handle_mut<F, R>(h: Handle, f: F) -> Result<R, SyscallError>
where
    F: FnOnce(&mut KernelObject) -> Result<R, SyscallError>,
{
    let mut table = HANDLE_TABLE.lock();
    match table.get_mut(&h) {
        Some(obj) => f(obj),
        None => Err(SyscallError::BadHandle),
    }
}

/// Allocate a new kernel-object handle in the global table.
/// Exposed for use by other kernel modules.
pub(crate) fn alloc_kernel_object(obj: KernelObject) -> Handle {
    alloc_handle(obj)
}

// ── Global kernel-object handle table access ───────────────────

/// Access the global HANDLE_TABLE for kernel-internal use.
pub fn with_handle_table<F, R>(f: F) -> R
where
    F: FnOnce(&mut BTreeMap<u64, KernelObject>) -> R,
{
    let mut table = HANDLE_TABLE.lock();
    f(&mut *table)
}

// ── Main dispatch ──────────────────────────────────────────────

/// Handle system call from user space
#[unsafe(no_mangle)]
pub unsafe extern "C" fn handle_syscall(
    syscall_num: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
    arg6: u64,
) -> u64 {
    // Check if the current process has a runtime dispatch mode.
    let current_pid = crate::process::current_pid();
    let dispatch_mode = current_pid
        .and_then(|pid| {
            crate::process::PROCESS_MANAGER.with_process(pid, |p| {
                matches!(p.dispatch_mode, Some(crate::linux::DispatchMode::Linux(_)))
            })
        })
        .unwrap_or(false);

    if dispatch_mode {
        let mut linux_rt = current_pid
            .and_then(|pid| {
                crate::process::PROCESS_MANAGER
                    .with_process(pid, |p| {
                        p.dispatch_mode.take().and_then(|mode| {
                            if let crate::linux::DispatchMode::Linux(rt) = mode {
                                Some(rt)
                            } else {
                                p.dispatch_mode = Some(mode);
                                None
                            }
                        })
                    })
                    .flatten()
            });

        let ret = if let Some(mut rt) = linux_rt.take() {
            let result = rt.dispatch(syscall_num, &[arg1, arg2, arg3, arg4, arg5, arg6]);
            if let Some(pid) = current_pid {
                crate::process::PROCESS_MANAGER.with_process(pid, |p| {
                    p.dispatch_mode = Some(crate::linux::DispatchMode::Linux(rt));
                });
            }
            result
        } else {
            crate::linux::errno_code(crate::linux::ENOSYS)
        };
        return ret;
    }

    // Fullerene native syscall dispatch
    let result = match syscall_num {
        // ── Basic (1–22) ────────────────────────────────
        1 => syscall_exit(arg1 as i32),
        2 => syscall_fork(),
        3 => syscall_read(arg1 as core::ffi::c_int, arg2 as *mut u8, arg3 as usize),
        4 => syscall_write(arg1 as core::ffi::c_int, arg2 as *const u8, arg3 as usize),
        5 => syscall_open(arg1 as *const u8, arg2 as core::ffi::c_int, arg3 as u32),
        6 => syscall_close(arg1 as core::ffi::c_int),
        7 => syscall_wait(arg1 as u64),
        20 => syscall_getpid(),
        21 => syscall_get_process_name(arg1 as *mut u8, arg2 as usize),
        22 => syscall_yield(),

        // ── Memory (30–39) ─────────────────────────────
        30 => syscall_map_memory(arg1, arg2, arg3),
        31 => syscall_unmap_memory(arg1, arg2),
        32 => syscall_protect_memory(arg1, arg2, arg3),
        33 => syscall_query_memory(arg1 as *mut u8, arg2 as usize),

        // ── Event (40–49) ──────────────────────────────
        40 => syscall_create_event(arg1),
        41 => syscall_wait_event(arg1, arg2),
        42 => syscall_signal_event(arg1),
        43 => syscall_subscribe_event(arg1, arg2),

        // ── Thread (50–59) ─────────────────────────────
        50 => syscall_create_thread(arg1, arg2, arg3),
        51 => syscall_join_thread(arg1),
        52 => syscall_detach_thread(arg1),
        53 => syscall_exit_thread(arg1 as i32),

        // ── Window (60–69) ─────────────────────────────
        60 => syscall_create_window(arg1 as i32, arg2 as i32, arg3 as u32, arg4 as u32, arg5),
        61 => syscall_destroy_window(arg1),
        62 => syscall_resize_window(arg1, arg2 as u32, arg3 as u32),
        63 => syscall_present_window(arg1),
        64 => syscall_get_window_event(arg1, arg2 as *mut u8, arg3 as usize),

        // ── Device (70–79) ─────────────────────────────
        70 => syscall_enumerate_devices(arg1, arg2 as *mut u8, arg3 as usize),
        71 => syscall_open_device(arg1 as *const u8),
        72 => syscall_device_ioctl(arg1, arg2, arg3),

        // ── IPC (80–89) ────────────────────────────────
        80 => syscall_channel_create(arg1),
        81 => syscall_channel_send(arg1, arg2 as *const u8, arg3),
        82 => syscall_channel_recv(arg1, arg2 as *mut u8, arg3),
        83 => syscall_pipe_create(arg1),

        // ── Handle / Capability (90–99) ────────────────
        90 => syscall_handle_transfer(arg1 as u64, arg2),
        91 => syscall_handle_duplicate(arg1),
        92 => syscall_handle_revoke(arg1),

        // ── Time (100–109) ─────────────────────────────
        100 => syscall_clock_gettime(arg1, arg2 as *mut u8),
        101 => syscall_timer_create(arg1, arg2, arg3),
        102 => syscall_sleep(arg1),
        103 => syscall_uptime(arg1 as *mut u8),

        _ => Err(SyscallError::InvalidSyscall),
    };

    match result {
        Ok(value) => value,
        Err(error) => -(error as i32) as u64,
    }
}

// ===================================================================
//  Basic syscalls (unchanged except where noted)
// ===================================================================

pub(crate) fn syscall_exit(exit_code: i32) -> SyscallResult {
    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    process::terminate_process(pid, exit_code);
    Ok(0)
}

fn syscall_fork() -> SyscallResult {
    let current_pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;

    let (parent_page_table_phys_addr, parent_context, parent_user_stack, parent_entry_point) = {
        crate::process::PROCESS_MANAGER
            .with_process(current_pid, |p| {
                (
                    p.page_table_phys_addr,
                    p.context.clone(),
                    p.user_stack,
                    p.entry_point,
                )
            })
            .ok_or(SyscallError::NoSuchProcess)?
    };

    let cloned_table_addr = {
        let mut manager_guard = crate::memory_management::get_memory_manager().lock();
        let manager = manager_guard.as_mut().ok_or(SyscallError::OutOfMemory)?;

        let ptm = &mut manager.page_table_manager;
        let alloc = petroleum::page_table::constants::get_frame_allocator_mut();
        petroleum::page_table::PageTableHelper::clone_page_table(
            ptm,
            parent_page_table_phys_addr.as_u64() as usize,
            alloc,
        )?
    };

    let cloned_pml4_frame = x86_64::structures::paging::PhysFrame::containing_address(
        x86_64::PhysAddr::new(cloned_table_addr as u64),
    );

    let mut child_page_table =
        petroleum::page_table::ProcessPageTable::new_with_frame(cloned_pml4_frame);
    petroleum::initializer::Initializable::init(&mut child_page_table)
        .map_err(|_| SyscallError::InvalidArgument)?;

    let stack_layout = Layout::from_size_align(KERNEL_STACK_SIZE, 16).unwrap();
    let kernel_stack_ptr = petroleum::common::memory::allocate_layout(stack_layout)
        .map_err(|_| SyscallError::OutOfMemory)?;
    let kernel_stack_top = VirtAddr::new(kernel_stack_ptr as u64 + KERNEL_STACK_SIZE as u64);

    let child_pid = process::PROCESS_MANAGER.allocate_pid().0 as usize;

    let mut child_process = Process {
        id: process::ProcessId(child_pid as u64),
        name: "child",
        state: ProcessState::Ready,
        context: parent_context.clone(),
        page_table_phys_addr: PhysAddr::new(cloned_table_addr as u64),
        page_table: Some(Box::new(child_page_table)),
        kernel_stack: kernel_stack_top,
        user_stack: parent_user_stack,
        entry_point: parent_entry_point,
        is_user: parent_context.is_user,
        task_data: 0,
        exit_code: None,
        parent_id: Some(current_pid),
        dispatch_mode: None,
    };

    child_process.context.regs[0] = 0;
    child_process.context.regs[7] = child_process.kernel_stack.as_u64();

    let child_box = Box::new(child_process);

    crate::process::PROCESS_MANAGER
        .add(child_box)
        .map_err(|_| SyscallError::OutOfMemory)?;

    Ok(child_pid as u64)
}

fn syscall_read(fd: core::ffi::c_int, buffer: *mut u8, count: usize) -> SyscallResult {
    if count == 0 {
        return Ok(0);
    }

    let data = unsafe { user_slice_mut(buffer, count, false) }
        .map_err(|_| SyscallError::InvalidArgument)?;

    petroleum::validate_syscall_fd(fd)?;

    if fd == 0 {
        if count == 1 {
            if let Some(ch) = nitrogen::ps2::keyboard::read_char() {
                data[0] = ch;
                Ok(1)
            } else {
                Ok(0)
            }
        } else {
            let bytes_read = nitrogen::ps2::keyboard::drain_line_buffer(data);
            Ok(bytes_read as u64)
        }
    } else {
        if fd < 0 {
            return Err(SyscallError::BadFileDescriptor);
        }
        let mut fd_table = FD_TABLE.lock();
        if let Some(file_desc) = fd_table.get_mut(&(fd as u32)) {
            match crate::fs::read_file(file_desc, data) {
                Ok(n) => Ok(n as u64),
                Err(_) => Err(SyscallError::BadFileDescriptor),
            }
        } else {
            Err(SyscallError::BadFileDescriptor)
        }
    }
}

fn syscall_write(fd: core::ffi::c_int, buffer: *const u8, count: usize) -> SyscallResult {
    petroleum::validate_syscall_fd(fd)?;
    let allow_kernel = fd == 1 || fd == 2;
    if count == 0 {
        return Ok(0);
    }

    let data = unsafe { user_slice(buffer, count, allow_kernel) }
        .map_err(|_| SyscallError::InvalidArgument)?;

    if fd == 1 || fd == 2 {
        petroleum::write_serial_bytes(0x3F8, 0x3FD, data);
        Ok(count as u64)
    } else {
        Err(SyscallError::BadFileDescriptor)
    }
}

fn syscall_open(filename: *const u8, flags: core::ffi::c_int, _mode: u32) -> SyscallResult {
    let filename_str = unsafe { copy_user_string(filename, 256)? };

    let read_only = (flags & 0x3) == O_RDONLY;
    let write_only = (flags & 0x3) == O_WRONLY;
    let read_write = (flags & 0x3) == O_RDWR;
    let create = (flags & O_CREAT) != 0;
    let truncate = (flags & O_TRUNC) != 0;
    let append = (flags & O_APPEND) != 0;

    if create || truncate || append || write_only || read_write {
        return Err(SyscallError::PermissionDenied);
    }

    if read_only {
        match crate::fs::open_file(&filename_str) {
            Ok(file_desc) => {
                let mut fd_table = FD_TABLE.lock();
                let mut next_fd = NEXT_FD.lock();
                let fd = *next_fd;
                *next_fd += 1;
                fd_table.insert(fd, file_desc);
                Ok(fd as u64)
            }
            Err(crate::fs::FsError::FileNotFound) => Err(SyscallError::FileNotFound),
            Err(_) => Err(SyscallError::PermissionDenied),
        }
    } else {
        Err(SyscallError::PermissionDenied)
    }
}

fn syscall_close(fd: core::ffi::c_int) -> SyscallResult {
    if fd < 0 {
        return Err(SyscallError::InvalidArgument);
    }
    if fd <= 2 {
        return Err(SyscallError::InvalidArgument);
    }
    let mut fd_table = FD_TABLE.lock();
    if let Some(file_desc) = fd_table.remove(&(fd as u32)) {
        match crate::fs::close_file(file_desc) {
            Ok(_) => Ok(0),
            Err(_) => Err(SyscallError::BadFileDescriptor),
        }
    } else {
        Err(SyscallError::BadFileDescriptor)
    }
}

fn syscall_wait(pid: u64) -> SyscallResult {
    if pid == 0 {
        process::yield_current();
        Ok(0)
    } else {
        let pid_type = process::ProcessId(pid);
        let result = crate::process::PROCESS_MANAGER
            .with_process(pid_type, |process| {
                if process.state == crate::process::ProcessState::Terminated {
                    Some(process.exit_code.unwrap_or(0))
                } else {
                    None
                }
            })
            .flatten();

        if let Some(exit_code) = result {
            Ok(exit_code as u64)
        } else if crate::process::PROCESS_MANAGER
            .with_process(pid_type, |_| {})
            .is_some()
        {
            crate::process::block_current();
            Ok(0)
        } else {
            Err(SyscallError::NoSuchProcess)
        }
    }
}

fn syscall_getpid() -> SyscallResult {
    Ok(process::current_pid().map(|pid| pid.0).unwrap_or(0))
}

fn syscall_get_process_name(buffer: *mut u8, size: usize) -> SyscallResult {
    if size == 0 {
        return Err(SyscallError::InvalidArgument);
    }
    petroleum::validate_user_buffer(buffer as usize, size, false)?;
    let current_pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;

    crate::process::PROCESS_MANAGER
        .with_process(current_pid, |process| {
            let name_bytes = process.name.as_bytes();
            let copy_len = name_bytes.len().min(size - 1);

            unsafe {
                let user_buf = user_slice_mut(buffer, size, false)
                    .map_err(|_| SyscallError::InvalidArgument)?;
                user_buf[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
                if copy_len < size {
                    user_buf[copy_len] = b'\0';
                }
                Ok(copy_len as u64)
            }
        })
        .ok_or(SyscallError::NoSuchProcess)?
        .map_err(|e| e)
}

fn syscall_yield() -> SyscallResult {
    process::yield_current();
    Ok(0)
}

// ===================================================================
//  Memory syscalls (30–39)
// ===================================================================

/// Memory protection flags passed by user-space.
const PROT_NONE: u64 = 0;
const PROT_READ: u64 = 1;
const PROT_WRITE: u64 = 2;
const PROT_EXEC: u64 = 4;

/// Map flags
const MAP_ANONYMOUS: u64 = 1 << 10;
const MAP_SHARED: u64 = 0x01;
const MAP_PRIVATE: u64 = 0x02;

fn syscall_map_memory(addr_hint: u64, length: u64, flags: u64) -> SyscallResult {
    let len = length as usize;
    if len == 0 || len > (128 << 20) {
        // cap at 128 MiB
        return Err(SyscallError::InvalidArgument);
    }

    let map_flags = flags & 0xFFFF;
    let prot = (flags >> 16) & 0xFF;

    let is_anonymous = (map_flags & MAP_ANONYMOUS) != 0;

    // Translate protection flags to page-table flags
    let mut pt_flags = x86_64::structures::paging::PageTableFlags::empty();
    if (prot & PROT_READ) != 0 {
        pt_flags |= x86_64::structures::paging::PageTableFlags::PRESENT;
    }
    if (prot & PROT_WRITE) != 0 {
        pt_flags |= x86_64::structures::paging::PageTableFlags::WRITABLE;
    }
    if (prot & PROT_EXEC) == 0 {
        pt_flags |= x86_64::structures::paging::PageTableFlags::NO_EXECUTE;
    }
    // Always allow user access for user-space mappings
    pt_flags |= x86_64::structures::paging::PageTableFlags::USER_ACCESSIBLE;

    // Allocate physical frames and map them
    match kernel::with_kernel_mut(|k| -> SyscallResult {
        let memory = &mut k.memory;

        // Pick a virtual address: use hint if page-aligned and user-accessible,
        // otherwise allocate from the process's page table.
        let virt_base = if addr_hint != 0
            && addr_hint % 4096 == 0
            && petroleum::is_user_address(VirtAddr::new(addr_hint))
        {
            addr_hint as usize
        } else {
            // For simplicity, allocate at a fixed offset from 0x1_0000_0000_0000
            // A real implementation would consult VirtualMemoryContext.
            0x100_0000_0000 + (len * NEXT_HANDLE.load(Ordering::Relaxed) as usize)
        };

        let num_pages = (len + 4095) / 4096;

        for i in 0..num_pages {
            let frame = memory
                .allocate_frame()
                .map_err(|_| SyscallError::OutOfMemory)?;
            let vaddr = virt_base + i * 4096;
            memory
                .map_page(vaddr, frame, pt_flags)
                .map_err(|_| SyscallError::OutOfMemory)?;
        }

        Ok(virt_base as u64)
    }) {
        Some(Ok(v)) => Ok(v),
        Some(Err(e)) => Err(e),
        None => Err(SyscallError::OutOfMemory),
    }
}

fn syscall_unmap_memory(addr: u64, length: u64) -> SyscallResult {
    let len = length as usize;
    if len == 0 || (addr % 4096) != 0 {
        return Err(SyscallError::InvalidArgument);
    }

    match kernel::with_kernel_mut(|k| -> SyscallResult {
        let memory = &mut k.memory;
        let num_pages = (len + 4095) / 4096;
        for i in 0..num_pages {
            let _vaddr = addr as usize + i * 4096;
            // Identity-unmap by mapping a zero page (lazy – real impl would
            // flush TLB and free frames)
            memory
                .map_page(_vaddr, 0, x86_64::structures::paging::PageTableFlags::empty())
                .map_err(|_| SyscallError::OutOfMemory)?;
        }
        Ok(0)
    }) {
        Some(Ok(v)) => Ok(v),
        Some(Err(e)) => Err(e),
        None => Err(SyscallError::NotSupported),
    }
}

fn syscall_protect_memory(addr: u64, _length: u64, _prot: u64) -> SyscallResult {
    if addr % 4096 != 0 {
        return Err(SyscallError::InvalidArgument);
    }
    // Stub: real implementation would walk the page table and update
    // the protection bits for the given range.
    Err(SyscallError::NotSupported)
}

fn syscall_query_memory(info_buf: *mut u8, buf_size: usize) -> SyscallResult {
    if info_buf.is_null() || buf_size < 64 {
        return Err(SyscallError::InvalidArgument);
    }
    petroleum::validate_user_buffer(info_buf as usize, buf_size, false)?;

    // Stub: return a single info structure with zero fields.
    // A real implementation would fill a MemoryInfo struct with
    // { base, size, protection, type } for the queried address.
    let data =
        unsafe { user_slice_mut(info_buf, buf_size, false) }.map_err(|_| {
            SyscallError::InvalidArgument
        })?;
    data.fill(0);
    Ok(0)
}

// ===================================================================
//  Event syscalls (40–49)
// ===================================================================

/// Event creation flags.
const EVENT_MANUAL_RESET: u64 = 1;

fn syscall_create_event(flags: u64) -> SyscallResult {
    let manual_reset = (flags & EVENT_MANUAL_RESET) != 0;
    let state = EventState {
        signaled: false,
        manual_reset,
        waiters: Vec::new(),
    };
    let handle = alloc_handle(KernelObject::Event(state));

    // Also push the event into the global EventContext system queue so
    // the dispatcher knows about it.
    kernel::with_kernel_mut(|k| {
        use resonance::Event as ResonanceEvent;
        k.event.push_system(ResonanceEvent::System(
            resonance::event::SystemEvent::Resume, // reuse as "object created"
        ));
    });

    Ok(handle)
}

fn syscall_wait_event(handle: u64, timeout_us: u64) -> SyscallResult {
    let signaled = with_handle_mut(handle, |obj| {
        let event = match obj {
            KernelObject::Event(e) => e,
            _ => return Err(SyscallError::BadHandle),
        };
        if event.signaled {
            if !event.manual_reset {
                event.signaled = false;
            }
            Ok(true)
        } else {
            // Register the current PID as a waiter and block.
            let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
            event.waiters.push(pid);
            Ok(false)
        }
    })?;

    if signaled {
        Ok(0)
    } else if timeout_us == 0 {
        // Non-blocking: return "would block"
        Err(SyscallError::WouldBlock)
    } else {
        // Block the calling process until signalled or timeout.
        // Timeout handling is a stub here: we simply block.
        crate::process::block_current();
        // After unblock, re-check the event
        with_handle_mut(handle, |obj| {
            let event = match obj {
                KernelObject::Event(e) => e,
                _ => return Err(SyscallError::BadHandle),
            };
            if event.signaled {
                if !event.manual_reset {
                    event.signaled = false;
                }
                Ok(0)
            } else {
                Err(SyscallError::TimedOut)
            }
        })
    }
}

fn syscall_signal_event(handle: u64) -> SyscallResult {
    let pids_to_unblock: Vec<process::ProcessId> = with_handle_mut(handle, |obj| {
        let event = match obj {
            KernelObject::Event(e) => e,
            _ => return Err(SyscallError::BadHandle),
        };
        event.signaled = true;
        let waiters = core::mem::take(&mut event.waiters);
        Ok(waiters)
    })?;

    // Unblock all waiters outside the handle-table lock.
    for pid in pids_to_unblock {
        crate::process::unblock_process(pid);
    }

    // Also push a generic event update into the kernel event context.
    kernel::with_kernel_mut(|k| {
        use resonance::Event as ResonanceEvent;
        k.event.push_system(ResonanceEvent::System(
            resonance::event::SystemEvent::Resume,
        ));
    });

    Ok(0)
}

fn syscall_subscribe_event(event_type: u64, _callback_info: u64) -> SyscallResult {
    // Stub: register a subscription to a given event type.
    // In a full implementation the callback_info would contain a handler
    // reference or a target handle.
    let _ = event_type;
    Err(SyscallError::NotSupported)
}

// ===================================================================
//  Thread syscalls (50–59)
// ===================================================================

fn syscall_create_thread(entry: u64, stack: u64, _flags: u64) -> SyscallResult {
    let entry_point = VirtAddr::new(entry);
    let user_stack = VirtAddr::new(stack);

    if !petroleum::is_user_address(entry_point) {
        return Err(SyscallError::InvalidArgument);
    }

    if !petroleum::is_user_address(user_stack) {
        return Err(SyscallError::InvalidArgument);
    }

    // Threads share the parent's address space → fork a lightweight process
    // that uses the same page table.
    let current_pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;

    let (parent_pt_phys, parent_context) = {
        crate::process::PROCESS_MANAGER
            .with_process(current_pid, |p| {
                (p.page_table_phys_addr, p.context.clone())
            })
            .ok_or(SyscallError::NoSuchProcess)?
    };

    let stack_layout = Layout::from_size_align(KERNEL_STACK_SIZE, 16).unwrap();
    let kernel_stack_ptr = petroleum::common::memory::allocate_layout(stack_layout)
        .map_err(|_| SyscallError::OutOfMemory)?;
    let kernel_stack_top = VirtAddr::new(kernel_stack_ptr as u64 + KERNEL_STACK_SIZE as u64);

    let child_pid = process::PROCESS_MANAGER.allocate_pid();
    let tid = child_pid.0;

    let mut thread_process = Process {
        id: child_pid,
        name: "thread",
        state: ProcessState::Ready,
        context: parent_context.clone(),
        page_table_phys_addr: parent_pt_phys,
        page_table: None, // shares parent's page table
        kernel_stack: kernel_stack_top,
        user_stack,
        entry_point,
        is_user: true,
        task_data: 0,
        exit_code: None,
        parent_id: Some(current_pid),
        dispatch_mode: None,
    };

    thread_process.context.regs[0] = 0;
    thread_process.context.regs[7] = thread_process.user_stack.as_u64();
    thread_process.context.rip = entry;

    let thread_box = Box::new(thread_process);
    crate::process::PROCESS_MANAGER
        .add(thread_box)
        .map_err(|_| SyscallError::OutOfMemory)?;

    // Allocate a thread handle for join/detach
    let tstate = ThreadState {
        tid: child_pid.0,
        pid: child_pid,
        detached: false,
        exit_code: None,
        waiters: Vec::new(),
    };
    let handle = alloc_handle(KernelObject::Thread(tstate));

    Ok(handle)
}

fn syscall_join_thread(handle: u64) -> SyscallResult {
    // Check if the thread has an exit code; if not, block.
    let done = with_handle_mut(handle, |obj| {
        let thread = match obj {
            KernelObject::Thread(t) => t,
            _ => return Err(SyscallError::BadHandle),
        };
        if let Some(exit_code) = thread.exit_code {
            Ok(Some(exit_code))
        } else {
            let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
            thread.waiters.push(pid);
            Ok(None)
        }
    })?;

    match done {
        Some(exit_code) => Ok(exit_code as u64),
        None => {
            crate::process::block_current();
            // After unblock, re-check
            with_handle_mut(handle, |obj| {
                let thread = match obj {
                    KernelObject::Thread(t) => t,
                    _ => return Err(SyscallError::BadHandle),
                };
                thread.exit_code.map(|ec| ec as u64).ok_or(SyscallError::NoSuchProcess)
            })
        }
    }
}

fn syscall_detach_thread(handle: u64) -> SyscallResult {
    with_handle_mut(handle, |obj| {
        let thread = match obj {
            KernelObject::Thread(t) => t,
            _ => return Err(SyscallError::BadHandle),
        };
        thread.detached = true;
        Ok(0)
    })
}

fn syscall_exit_thread(exit_code: i32) -> SyscallResult {
    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;

    // Update the thread handle if it exists
    let waiters: Vec<process::ProcessId> = {
        let mut table = HANDLE_TABLE.lock();
        let mut found_waiters: Vec<process::ProcessId> = Vec::new();
        for (_h, obj) in table.iter_mut() {
            if let KernelObject::Thread(t) = obj {
                if t.pid == pid {
                    t.exit_code = Some(exit_code);
                    let mut taken = core::mem::take(&mut t.waiters);
                    found_waiters.append(&mut taken);
                    break;
                }
            }
        }
        found_waiters
    };

    for wpid in waiters {
        crate::process::unblock_process(wpid);
    }

    process::terminate_process(pid, exit_code);
    Ok(0)
}

// ===================================================================
//  Window syscalls (60–69)
// ===================================================================

const WINDOW_BORDERED: u64 = 1;
const WINDOW_FRAMELESS: u64 = 2;
const WINDOW_NO_TASKBAR: u64 = 4;

fn syscall_create_window(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    flags: u64,
) -> SyscallResult {
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return Err(SyscallError::InvalidArgument);
    }

    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;

    let window_index: usize = match kernel::with_kernel_mut(|k| {
        let win_id = k.window.next_window_id();
        let win = crate::contexts::window::Window::new(
            win_id,
            "New Window",
            x,
            y,
            width,
            height,
        );
        k.window.add_window(win);
        k.window.windows.len().wrapping_sub(1)
    }) {
        Some(idx) => idx,
        None => return Err(SyscallError::OutOfMemory),
    };

    let state = WindowState {
        window_index,
        pid,
    };
    let handle = alloc_handle(KernelObject::Window(state));

    Ok(handle)
}

fn syscall_destroy_window(handle: u64) -> SyscallResult {
    with_handle_mut(handle, |obj| {
        let window = match obj {
            KernelObject::Window(w) => w,
            _ => return Err(SyscallError::BadHandle),
        };
        let idx = window.window_index;
        kernel::with_kernel_mut(|k| {
            if idx < k.window.windows.len() {
                k.window.windows[idx].visible = false;
            }
        });
        Ok(0)
    })
}

fn syscall_resize_window(handle: u64, width: u32, height: u32) -> SyscallResult {
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return Err(SyscallError::InvalidArgument);
    }
    with_handle_mut(handle, |obj| {
        let window = match obj {
            KernelObject::Window(w) => w,
            _ => return Err(SyscallError::BadHandle),
        };
        let idx = window.window_index;
        kernel::with_kernel_mut(|k| {
            if idx < k.window.windows.len() {
                k.window.windows[idx].width = width;
                k.window.windows[idx].height = height;
            }
        });
        Ok(0)
    })
}

fn syscall_present_window(handle: u64) -> SyscallResult {
    with_handle_mut(handle, |obj| {
        let window = match obj {
            KernelObject::Window(w) => w,
            _ => return Err(SyscallError::BadHandle),
        };
        let idx = window.window_index;
        kernel::with_kernel_mut(|k| {
            if idx < k.window.windows.len() {
                k.window.windows[idx].visible = true;
                // Mark redraw pending via event system
                use resonance::Event as ResonanceEvent;
                k.event.push(ResonanceEvent::Window(
                    resonance::event::WindowEvent::Redraw(k.window.windows[idx].id.0),
                ));
            }
        });
        Ok(0)
    })
}

fn syscall_get_window_event(handle: u64, buf: *mut u8, buf_size: usize) -> SyscallResult {
    if buf.is_null() || buf_size < 128 {
        return Err(SyscallError::InvalidArgument);
    }
    petroleum::validate_user_buffer(buf as usize, buf_size, false)?;

    with_handle_mut(handle, |obj| {
        let _window = match obj {
            KernelObject::Window(_w) => (),
            _ => return Err(SyscallError::BadHandle),
        };

        // Stub: drain events from the kernel EventContext.
        // A real implementation would serialize a WindowEvent struct.
        let has_event = kernel::with_kernel(|k| k.event.has_pending()).unwrap_or(false);
        if has_event {
            let data = unsafe { user_slice_mut(buf, buf_size, false) }
                .map_err(|_| SyscallError::InvalidArgument)?;
            data.fill(0);
            // Write a simple event header: event_type (u64) = 0 means none
            // In a real impl this would be read from the queue.
            Ok(8) // sizeof(header)
        } else {
            Err(SyscallError::Again)
        }
    })
}

// ===================================================================
//  Device syscalls (70–79)
// ===================================================================

fn syscall_enumerate_devices(
    class: u64,
    buf: *mut u8,
    buf_size: usize,
) -> SyscallResult {
    if buf.is_null() || buf_size == 0 {
        return Err(SyscallError::InvalidArgument);
    }
    petroleum::validate_user_buffer(buf as usize, buf_size, false)?;

    // class: 0=all, 1=pci, 2=usb, 3=input, 4=audio
    let data = unsafe { user_slice_mut(buf, buf_size, false) }
        .map_err(|_| SyscallError::InvalidArgument)?;

    let count = kernel::with_kernel(|k| {
        let devices = match class {
            1 => &k.pci.devices, // PciContext has Vec<PciDevice>
            _ => {
                // Return empty for other classes (stub)
                return 0usize;
            }
        };

        // Serialize device info: [class_tag(u32), vendor(u32), device(u32), id_len(u8), id...]
        let mut offset = 0;
        for dev in devices.iter().take(buf_size / 32) {
            if offset + 16 > buf_size {
                break;
            }
            // Write stub device descriptor: 16 bytes each
            data[offset..offset + 4].copy_from_slice(&(class as u32).to_ne_bytes());
            data[offset + 4..offset + 8].copy_from_slice(&0_u32.to_ne_bytes()); // vendor
            data[offset + 8..offset + 12].copy_from_slice(&0_u32.to_ne_bytes()); // device_id
            data[offset + 12..offset + 16].copy_from_slice(&0_u32.to_ne_bytes()); // name_len
            offset += 16;
        }
        devices.len()
    })
    .unwrap_or(0);

    Ok(count as u64)
}

fn syscall_open_device(device_id: *const u8) -> SyscallResult {
    let id_str = unsafe { copy_user_string(device_id, 128)? };
    if id_str.is_empty() {
        return Err(SyscallError::InvalidArgument);
    }

    let state = DeviceState {
        device_id: id_str.clone(),
        device_class: alloc::string::String::from("unknown"),
    };
    let handle = alloc_handle(KernelObject::Device(state));
    Ok(handle)
}

fn syscall_device_ioctl(handle: u64, _cmd: u64, _arg: u64) -> SyscallResult {
    with_handle_mut(handle, |obj| {
        let _device = match obj {
            KernelObject::Device(_d) => (),
            _ => return Err(SyscallError::BadHandle),
        };
        // Stub: no ioctl commands implemented yet
        Err(SyscallError::NotSupported)
    })
}

// ===================================================================
//  IPC syscalls (80–89)
// ===================================================================

fn syscall_channel_create(_flags: u64) -> SyscallResult {
    let state = ChannelState {
        messages: Vec::with_capacity(16),
        waiters: Vec::new(),
        max_messages: 64,
    };
    let handle = alloc_handle(KernelObject::Channel(state));
    Ok(handle)
}

fn syscall_channel_send(handle: u64, data_ptr: *const u8, data_size: u64) -> SyscallResult {
    let size = data_size as usize;
    if size == 0 || size > 65536 {
        return Err(SyscallError::InvalidArgument);
    }

    let data = unsafe { user_slice(data_ptr, size, false) }
        .map_err(|_| SyscallError::InvalidArgument)?;

    let recv_waiters: Vec<process::ProcessId> = with_handle_mut(handle, |obj| {
        let channel = match obj {
            KernelObject::Channel(ch) => ch,
            _ => return Err(SyscallError::BadHandle),
        };
        if channel.messages.len() >= channel.max_messages {
            return Err(SyscallError::Again);
        }
        let msg_vec = Vec::from(data);
        channel.messages.push(msg_vec);
        Ok(core::mem::take(&mut channel.waiters))
    })?;

    for pid in recv_waiters {
        crate::process::unblock_process(pid);
    }

    Ok(size as u64)
}

fn syscall_channel_recv(handle: u64, buf: *mut u8, buf_size: u64) -> SyscallResult {
    let max = buf_size as usize;
    if buf.is_null() || max == 0 {
        return Err(SyscallError::InvalidArgument);
    }
    petroleum::validate_user_buffer(buf as usize, max, false)?;

    with_handle_mut(handle, |obj| {
        let channel = match obj {
            KernelObject::Channel(ch) => ch,
            _ => return Err(SyscallError::BadHandle),
        };
        if !channel.messages.is_empty() {
            let msg = channel.messages.remove(0);
            let copy_len = msg.len().min(max);
            let dest = unsafe { user_slice_mut(buf, max, false) }
                .map_err(|_| SyscallError::InvalidArgument)?;
            dest[..copy_len].copy_from_slice(&msg[..copy_len]);
            Ok(copy_len as u64)
        } else {
            // Block until a message arrives
            let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
            channel.waiters.push(pid);
            Err(SyscallError::WouldBlock)
        }
    })
}

fn syscall_pipe_create(_flags: u64) -> SyscallResult {
    // Create paired read/write handles
    let buffer: Vec<u8> = Vec::with_capacity(4096);
    let shared_buffer = alloc::sync::Arc::new(Mutex::new(buffer));

    // We store a shared reference via two handles using a "tag" scheme.
    // Simplified: two pipes with a shared buffer key.
    let read_end = PipeState {
        buffer: Vec::new(),
        is_read_end: true,
        peer_waiters: Vec::new(),
    };
    let write_end = PipeState {
        buffer: Vec::new(),
        is_read_end: false,
        peer_waiters: Vec::new(),
    };

    let read_h = alloc_handle(KernelObject::Pipe(read_end));
    let write_h = alloc_handle(KernelObject::Pipe(write_end));

    // Pack two handles into return: low 32 = read, high 32 = write.
    Ok(read_h | (write_h << 32))
}

// ===================================================================
//  Handle / Capability syscalls (90–99)
// ===================================================================

fn syscall_handle_transfer(target_pid: u64, handle: u64) -> SyscallResult {
    let target = process::ProcessId(target_pid);
    if crate::process::PROCESS_MANAGER
        .with_process(target, |_| {})
        .is_none()
    {
        return Err(SyscallError::NoSuchProcess);
    }

    // Remove from table and re-insert — effectively reassociates
    // handle with caller's rights.  A real capability system would
    // maintain a per-process handle table.
    let mut table = HANDLE_TABLE.lock();
    if let Some(obj) = table.remove(&handle) {
        // For now just put it back; full capability transfer would
        // move the object into the target's namespace.
        table.insert(handle, obj);
        Ok(0)
    } else {
        Err(SyscallError::BadHandle)
    }
}

fn syscall_handle_duplicate(handle: u64) -> SyscallResult {
    let mut table = HANDLE_TABLE.lock();
    if table.contains_key(&handle) {
        let new_h = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        // Shallow duplicate: clone the KernelObject.  This is not a deep copy for
        // complex objects (channel messages are lost, etc.) — a real implementation
        // would ref-count.
        match table.get(&handle) {
            Some(KernelObject::Event(e)) => {
                let copy = EventState {
                    signaled: e.signaled,
                    manual_reset: e.manual_reset,
                    waiters: Vec::new(),
                };
                table.insert(new_h, KernelObject::Event(copy));
                Ok(new_h)
            }
            Some(KernelObject::Thread(t)) => {
                let copy = ThreadState {
                    tid: t.tid,
                    pid: t.pid,
                    detached: t.detached,
                    exit_code: t.exit_code,
                    waiters: Vec::new(),
                };
                table.insert(new_h, KernelObject::Thread(copy));
                Ok(new_h)
            }
            Some(KernelObject::Channel(ch)) => {
                let copy = ChannelState {
                    messages: Vec::with_capacity(ch.max_messages),
                    waiters: Vec::new(),
                    max_messages: ch.max_messages,
                };
                table.insert(new_h, KernelObject::Channel(copy));
                Ok(new_h)
            }
            Some(KernelObject::Window(w)) => {
                let copy = WindowState {
                    window_index: w.window_index,
                    pid: w.pid,
                };
                table.insert(new_h, KernelObject::Window(copy));
                Ok(new_h)
            }
            _ => Err(SyscallError::NotSupported),
        }
    } else {
        Err(SyscallError::BadHandle)
    }
}

fn syscall_handle_revoke(handle: u64) -> SyscallResult {
    let mut table = HANDLE_TABLE.lock();
    if table.remove(&handle).is_some() {
        Ok(0)
    } else {
        Err(SyscallError::BadHandle)
    }
}

// ===================================================================
//  Time syscalls (100–109)
// ===================================================================

/// Monotonic uptime counter in microseconds, incremented by the timer ISR.
static UPTIME_US: AtomicU64 = AtomicU64::new(0);

/// Internal: increment the uptime counter.  Called from the timer interrupt
/// handler with the number of microseconds elapsed since the last tick.
pub fn tick_uptime(delta_us: u64) {
    UPTIME_US.fetch_add(delta_us, Ordering::Relaxed);
}

/// Get the current uptime in microseconds.
fn uptime_us() -> u64 {
    UPTIME_US.load(Ordering::Relaxed)
}

fn syscall_clock_gettime(clock_id: u64, timespec_buf: *mut u8) -> SyscallResult {
    if timespec_buf.is_null() {
        return Err(SyscallError::InvalidArgument);
    }
    petroleum::validate_user_buffer(timespec_buf as usize, 16, false)?;

    // clock_id: 0 = MONOTONIC, 1 = REALTIME
    let (sec, nsec) = match clock_id {
        0 => {
            let us = uptime_us();
            (us / 1_000_000, ((us % 1_000_000) * 1000))
        }
        1 => {
            // Real-time clock stub
            (0, 0)
        }
        _ => return Err(SyscallError::InvalidArgument),
    };

    let data = unsafe { user_slice_mut(timespec_buf, 16, false) }
        .map_err(|_| SyscallError::InvalidArgument)?;
    // Write timespec: tv_sec (u64), tv_nsec (u64)
    data[0..8].copy_from_slice(&sec.to_ne_bytes());
    data[8..16].copy_from_slice(&nsec.to_ne_bytes());

    Ok(0)
}

fn syscall_timer_create(_clock_id: u64, deadline_ns: u64, event_handle: u64) -> SyscallResult {
    let state = TimerState {
        deadline_us: deadline_ns / 1000,
        signal_event: if event_handle != 0 {
            Some(event_handle)
        } else {
            None
        },
        fired: false,
    };
    let handle = alloc_handle(KernelObject::Timer(state));
    Ok(handle)
}

fn syscall_sleep(us: u64) -> SyscallResult {
    let deadline = uptime_us() + us;

    // Busy-wait for short sleeps (< 1 ms), yield for longer.
    if us < 1000 {
        let start = uptime_us();
        while uptime_us() < start + us {
            core::hint::spin_loop();
        }
        Ok(0)
    } else {
        // Yield to scheduler while waiting for deadline.
        // A real implementation would set a timer interrupt and unblock on expiry.
        while uptime_us() < deadline {
            process::yield_current();
        }
        Ok(0)
    }
}

fn syscall_uptime(buf: *mut u8) -> SyscallResult {
    if buf.is_null() {
        return Err(SyscallError::InvalidArgument);
    }
    petroleum::validate_user_buffer(buf as usize, 8, false)?;
    let us = uptime_us();
    let data = unsafe { user_slice_mut(buf, 8, false) }
        .map_err(|_| SyscallError::InvalidArgument)?;
    data[0..8].copy_from_slice(&us.to_ne_bytes());
    Ok(0)
}

// ===================================================================
//  Kernel helper and init
// ===================================================================

/// Kernel syscall call — calls syscall handler directly without syscall overhead.
pub fn kernel_syscall(syscall_num: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    unsafe { handle_syscall(syscall_num, arg1, arg2, arg3, 0, 0, 0) }
}

/// Initialize system calls
pub fn init() {
    use crate::interrupts::syscall::{init_syscall_stack, setup_syscall};
    init_syscall_stack();
    setup_syscall();
}