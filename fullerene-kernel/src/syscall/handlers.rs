use super::interface::{SyscallError, SyscallResult, copy_user_string};
use crate::process;
use crate::process::{Process, ProcessResources, ProcessState};
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::sync::atomic::{AtomicU64, Ordering};

use petroleum::common::memory::{user_slice, user_slice_mut};
use petroleum::page_table::types::PageTableHelper;
use spin::Mutex;
use x86_64::{PhysAddr, VirtAddr};

use crate::contexts::kernel;
use crate::linux::{O_APPEND, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY};

use resonance::Event as ResonanceEvent;

/// Opaque kernel-object handle exposed to user-space.
pub type Handle = u64;

// ── Per-process helpers ────────────────────────────────────────

/// Retrieve the current process's fd table and run a closure on it.
fn with_current_fd_table<F, R>(f: F) -> Result<R, SyscallError>
where
    F: FnOnce(&mut process::FdTable) -> Result<R, SyscallError>,
{
    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    match process::PROCESS_MANAGER.with_process(pid, |p| {
        let mut ft = p.resources.fd_table.lock();
        f(&mut *ft)
    }) {
        Some(r) => r,
        None => Err(SyscallError::NoSuchProcess),
    }
}

/// Retrieve the current process's handle table and run a closure on it.
fn with_current_handle_table<F, R>(f: F) -> Result<R, SyscallError>
where
    F: FnOnce(&mut crate::process::HandleTable) -> Result<R, SyscallError>,
{
    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    match process::PROCESS_MANAGER.with_process(pid, |p| {
        let mut ht = p.resources.handle_table.lock();
        f(&mut *ht)
    }) {
        Some(r) => r,
        None => Err(SyscallError::NoSuchProcess),
    }
}

/// Helper: call `with_kernel_mut` and unwrap through `Option<SyscallResult>`.
fn with_kernel_mut_result<F>(f: F) -> SyscallResult
where
    F: FnOnce(&mut crate::contexts::KernelContext) -> SyscallResult,
{
    crate::contexts::kernel::with_kernel_mut(f).ok_or(SyscallError::NotSupported)?
}

/// Extract a specific `KernelObject` variant or return `BadHandle` from the enclosing closure.
macro_rules! map_handle {
    ($obj:expr, $variant:ident, $name:ident) => {
        match $obj {
            KernelObject::$variant($name) => $name,
            _ => return Err(SyscallError::BadHandle),
        }
    };
}

/// Allocate a new handle in the current process's handle table.
fn alloc_handle(obj: KernelObject) -> Result<Handle, SyscallError> {
    with_current_handle_table(|ht| {
        let h = ht.next_handle;
        ht.next_handle = ht.next_handle.checked_add(1).ok_or(SyscallError::OutOfMemory)?;
        ht.entries.insert(h, obj);
        Ok(h)
    })
}

/// Run a closure on a handle in the current process's handle table.
fn with_handle_mut<F, R>(h: Handle, f: F) -> Result<R, SyscallError>
where
    F: FnOnce(&mut KernelObject) -> Result<R, SyscallError>,
{
    with_current_handle_table(|ht| match ht.entries.get_mut(&h) {
        Some(obj) => f(obj),
        None => Err(SyscallError::BadHandle),
    })
}

/// Set of kernel objects that can be referenced by a [`Handle`].
pub enum KernelObject {
    Event(EventState),
    Thread(ThreadState),
    Window(WindowState),
    Device(DeviceState),
    Channel(ChannelState),
    Pipe(PipeState),
    Timer(TimerState),
}

// ── Per-object state types ─────────────────────────────────────

/// Inner state shared between duplicated event handles.
pub struct EventInner {
    pub signaled: bool,
    pub manual_reset: bool,
    pub waiters: Vec<process::ProcessId>,
}

pub struct EventState {
    /// Shared inner state so duplicated handles see the same event.
    pub inner: alloc::sync::Arc<Mutex<EventInner>>,
}

/// Inner state shared between duplicated thread handles.
pub struct ThreadInner {
    pub pid: process::ProcessId,
    pub detached: bool,
    pub exit_code: Option<i32>,
    pub waiters: Vec<process::ProcessId>,
}

pub struct ThreadState {
    /// Shared inner state so duplicated handles see the same thread state.
    pub inner: alloc::sync::Arc<Mutex<ThreadInner>>,
}

pub struct WindowState {
    pub window_id: crate::contexts::window::WindowId,
    pub pid: process::ProcessId,
}

pub struct DeviceState {}

pub struct ChannelInner {
    pub messages: Vec<Vec<u8>>,
    pub waiters: Vec<process::ProcessId>,
    pub max_messages: usize,
}

pub struct ChannelState {
    pub inner: alloc::sync::Arc<Mutex<ChannelInner>>,
}

pub struct PipeState {
    pub buffer: alloc::sync::Arc<Mutex<Vec<u8>>>,
    pub is_read_end: bool,
}

pub struct TimerState {
    pub deadline_ns: u64,
    pub event_handle: Handle,
    pub fired: bool,
}

const KERNEL_STACK_SIZE: usize = 4096;

/// Allocate a kernel stack of `KERNEL_STACK_SIZE` bytes. Returns (ptr, top_virt).
/// Returns `Err(OutOfMemory)` if allocation fails.
fn alloc_kernel_stack() -> Result<(*mut u8, VirtAddr), SyscallError> {
    let layout = Layout::from_size_align(KERNEL_STACK_SIZE, 16).unwrap();
    let ptr = petroleum::common::memory::allocate_layout(layout)
        .map_err(|_| SyscallError::OutOfMemory)?;
    let top = VirtAddr::new(ptr as u64 + KERNEL_STACK_SIZE as u64);
    Ok((ptr, top))
}

/// Free a kernel stack allocated by `alloc_kernel_stack`.
fn free_kernel_stack(ptr: *mut u8) {
    let layout = Layout::from_size_align(KERNEL_STACK_SIZE, 16).unwrap();
    petroleum::common::memory::deallocate_layout(ptr, layout);
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
        let mut linux_rt = current_pid.and_then(|pid| {
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

    let (kernel_stack_ptr, kernel_stack_top) = alloc_kernel_stack()?;

    let child_pid = process::PROCESS_MANAGER.allocate_pid().0 as usize;

    // Remove inherited VDSO mapping (parent may have one at VDSO_USER_BASE)
    let _ = child_page_table.unmap_page(petroleum::vdso::VDSO_USER_BASE as usize);

    // Create child VDSO page
    let child_vdso = if parent_context.is_user {
        let mut fa_lock = crate::heap::FRAME_ALLOCATOR.lock();
        let fa = match fa_lock.as_mut() {
            Some(fa) => fa,
            None => {
                drop(fa_lock);
                free_kernel_stack(kernel_stack_ptr);
                crate::memory_management::deallocate_process_page_table(cloned_pml4_frame);
                return Err(SyscallError::OutOfMemory);
            }
        };
        let vdso = crate::vdso::create_vdso_page(&mut child_page_table, fa, child_pid as u64);
        drop(fa_lock);
        match vdso {
            Ok(v) => Some(v),
            Err(_) => {
                free_kernel_stack(kernel_stack_ptr);
                crate::memory_management::deallocate_process_page_table(cloned_pml4_frame);
                return Err(SyscallError::OutOfMemory);
            }
        }
    } else {
        None
    };

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
        vdso_page: child_vdso,
        resources: process::ProcessResources::new(),
    };

    child_process.context.regs[0] = 0;
    child_process.context.regs[7] = child_process.user_stack.as_u64();

    let child_box = Box::new(child_process);

    crate::process::PROCESS_MANAGER
        .add(child_box)
        .map_err(|_| {
            free_kernel_stack(kernel_stack_ptr);
            crate::memory_management::deallocate_process_page_table(cloned_pml4_frame);
            SyscallError::OutOfMemory
        })?;

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
        with_current_fd_table(|ft| {
            match ft.entries.get_mut(&(fd as u32)) {
                Some(file_desc) => match crate::fs::read_file(file_desc, data) {
                    Ok(n) => Ok(n as u64),
                    Err(_) => Err(SyscallError::BadFileDescriptor),
                },
                None => Err(SyscallError::BadFileDescriptor),
            }
        })
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
            Ok(file_desc) => with_current_fd_table(|ft| {
                let fd = ft.next_fd;
                ft.next_fd = ft.next_fd.checked_add(1).ok_or(SyscallError::OutOfMemory)?;
                ft.entries.insert(fd, file_desc);
                Ok(fd as u64)
            }),
            Err(crate::fs::FsError::FileNotFound) => Err(SyscallError::FileNotFound),
            Err(_) => Err(SyscallError::PermissionDenied),
        }
    } else {
        Err(SyscallError::PermissionDenied)
    }
}

fn syscall_close(fd: core::ffi::c_int) -> SyscallResult {
    if fd <= 2 {
        return Err(SyscallError::InvalidArgument);
    }
    with_current_fd_table(|ft| {
        match ft.entries.remove(&(fd as u32)) {
            Some(file_desc) => match crate::fs::close_file(file_desc) {
                Ok(_) => Ok(0),
                Err(_) => Err(SyscallError::BadFileDescriptor),
            },
            None => Err(SyscallError::BadFileDescriptor),
        }
    })
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
}

fn syscall_yield() -> SyscallResult {
    process::yield_current();
    Ok(0)
}

// ===================================================================
//  Memory syscalls (30–39)
// ===================================================================

const PROT_READ: u64 = 1;
const PROT_WRITE: u64 = 2;
const PROT_EXEC: u64 = 4;

/// Helper: unmap+free all pages in `pages` on a partial failure during
/// [`syscall_map_memory`].
fn rollback_mapped_pages(memory: &mut crate::contexts::memory::MemoryContext, pages: &[usize]) {
    if let Some(mgr) = memory.manager.as_mut() {
        for vaddr in pages {
            let _ = mgr.safe_unmap_page(*vaddr);
        }
    }
}

fn syscall_map_memory(addr_hint: u64, length: u64, flags: u64) -> SyscallResult {
    let len = length as usize;
    if len == 0 || len > (128 << 20) {
        // cap at 128 MiB
        return Err(SyscallError::InvalidArgument);
    }

    if addr_hint != 0 {
        let end_vaddr = addr_hint
            .checked_add(length)
            .ok_or(SyscallError::InvalidArgument)?;
        let start_addr = VirtAddr::new(addr_hint);
        let end_addr = VirtAddr::new(end_vaddr - 1);
        if !petroleum::is_user_address(start_addr) || !petroleum::is_user_address(end_addr) {
            return Err(SyscallError::PermissionDenied);
        }
    }

    let prot = (flags >> 16) & 0xFF;

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
    with_kernel_mut_result(|k| -> SyscallResult {
        let memory = &mut k.memory;

        let virt_base = if addr_hint != 0
            && addr_hint % 4096 == 0
            && petroleum::is_user_address(VirtAddr::new(addr_hint))
        {
            addr_hint as usize
        } else {
            static NEXT_VADDR: AtomicU64 = AtomicU64::new(0x100_0000_0000);
            let aligned_len = (len + 4095) & !4095;
            NEXT_VADDR.fetch_add(aligned_len as u64, Ordering::Relaxed) as usize
        };

        let num_pages = (len + 4095) / 4096;
        let mut mapped_pages: Vec<usize> = Vec::with_capacity(num_pages);
        for i in 0..num_pages {
            let frame = memory.allocate_frame().map_err(|_| {
                rollback_mapped_pages(memory, &mapped_pages);
                SyscallError::OutOfMemory
            })?;
            let vaddr = virt_base + i * 4096;
            memory.map_page(vaddr, frame, pt_flags).map_err(|_| {
                let _ = memory.free_frame(frame);
                rollback_mapped_pages(memory, &mapped_pages);
                SyscallError::OutOfMemory
            })?;
            mapped_pages.push(vaddr);
        }

        Ok(virt_base as u64)
    })
}

fn syscall_unmap_memory(addr: u64, length: u64) -> SyscallResult {
    let len = length as usize;
    if len == 0 || (addr % 4096) != 0 {
        return Err(SyscallError::InvalidArgument);
    }
    let end_vaddr = addr
        .checked_add(length)
        .ok_or(SyscallError::InvalidArgument)?;
    // Validate that the entire range is in user space.
    let start_addr = VirtAddr::new(addr);
    let end_addr = VirtAddr::new(end_vaddr - 1);
    if !petroleum::is_user_address(start_addr) || !petroleum::is_user_address(end_addr) {
        return Err(SyscallError::PermissionDenied);
    }

    with_kernel_mut_result(|k| -> SyscallResult {
        let memory = &mut k.memory;
        let num_pages = (len + 4095) / 4096;
        let mgr = memory.manager.as_mut().ok_or(SyscallError::OutOfMemory)?;
        for i in 0..num_pages {
            let vaddr = addr as usize + i * 4096;
            mgr.safe_unmap_page(vaddr)
                .map_err(|_| SyscallError::OutOfMemory)?;
        }
        Ok(0)
    })
}

fn syscall_protect_memory(_addr: u64, _length: u64, _prot: u64) -> SyscallResult {
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
    let data = unsafe { user_slice_mut(info_buf, buf_size, false) }
        .map_err(|_| SyscallError::InvalidArgument)?;
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
    let inner = alloc::sync::Arc::new(Mutex::new(EventInner {
        signaled: false,
        manual_reset,
        waiters: Vec::new(),
    }));
    let handle = alloc_handle(KernelObject::Event(EventState { inner }))?;

    // Also push the event into the global EventContext system queue so
    // the dispatcher knows about it.
    kernel::with_kernel_mut(|k| {
        k.event.push_system(ResonanceEvent::System(
            resonance::event::SystemEvent::Resume, // reuse as "object created"
        ));
    });

    Ok(handle)
}

fn syscall_wait_event(handle: u64, timeout_us: u64) -> SyscallResult {
    let signaled = with_handle_mut(handle, |obj| {
        let event = map_handle!(obj, Event, e);
        let mut inner = event.inner.lock();
        if inner.signaled {
            if !inner.manual_reset {
                inner.signaled = false;
            }
            Ok(true)
        } else {
            if timeout_us == 0 {
                return Ok(false);
            }
            let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
            inner.waiters.push(pid);
            Ok(false)
        }
    })?;

    if signaled {
        Ok(0)
    } else if timeout_us == 0 {
        Err(SyscallError::WouldBlock)
    } else {
        crate::process::block_current();
        with_handle_mut(handle, |obj| {
            let event = map_handle!(obj, Event, e);
            let mut inner = event.inner.lock();
            if inner.signaled {
                if !inner.manual_reset {
                    inner.signaled = false;
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
        let event = map_handle!(obj, Event, e);
        let mut inner = event.inner.lock();
        inner.signaled = true;
        let waiters = core::mem::take(&mut inner.waiters);
        Ok(waiters)
    })?;

    // Unblock all waiters outside the handle-table lock.
    for pid in pids_to_unblock {
        crate::process::unblock_process(pid);
    }

    // Also push a generic event update into the kernel event context.
    kernel::with_kernel_mut(|k| {
        k.event.push_system(ResonanceEvent::System(
            resonance::event::SystemEvent::Resume,
        ));
    });

    Ok(0)
}

fn syscall_subscribe_event(_event_type: u64, _callback_info: u64) -> SyscallResult {
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
    //
    // NOTE: The child process shares the parent's page table.  If the parent
    // terminates first and deallocates the PML4 frame, this thread will fault
    // when scheduled.  A proper fix requires reference-counting the page
    // table (e.g. Arc) or terminating all child threads before deallocating
    // the parent's page table.
    let current_pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;

    let (parent_pt_phys, parent_context) = {
        crate::process::PROCESS_MANAGER
            .with_process(current_pid, |p| (p.page_table_phys_addr, p.context.clone()))
            .ok_or(SyscallError::NoSuchProcess)?
    };

    let (kernel_stack_ptr, kernel_stack_top) = alloc_kernel_stack()?;

    let child_pid = process::PROCESS_MANAGER.allocate_pid();

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
        vdso_page: None, // shares parent's VDSO via shared page table
        resources: process::ProcessResources::new(),
    };

    thread_process.context.regs[0] = 0;
    thread_process.context.regs[7] = thread_process.user_stack.as_u64();
    thread_process.context.rip = entry;

    let thread_box = Box::new(thread_process);
    crate::process::PROCESS_MANAGER
        .add(thread_box)
        .map_err(|_| {
            free_kernel_stack(kernel_stack_ptr);
            SyscallError::OutOfMemory
        })?;

    // Allocate a thread handle for join/detach
    let inner = alloc::sync::Arc::new(Mutex::new(ThreadInner {
        pid: child_pid,
        detached: false,
        exit_code: None,
        waiters: Vec::new(),
    }));
    Ok(alloc_handle(KernelObject::Thread(ThreadState { inner }))?)
}

fn syscall_join_thread(handle: u64) -> SyscallResult {
    let done = with_handle_mut(handle, |obj| {
        let thread = map_handle!(obj, Thread, t);
        let mut inner = thread.inner.lock();
        if let Some(exit_code) = inner.exit_code {
            Ok(Some(exit_code))
        } else {
            let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
            inner.waiters.push(pid);
            Ok(None)
        }
    })?;

    match done {
        Some(exit_code) => Ok(exit_code as u64),
        None => {
            crate::process::block_current();
            with_handle_mut(handle, |obj| {
                let thread = map_handle!(obj, Thread, t);
                let inner = thread.inner.lock();
                inner
                    .exit_code
                    .map(|ec| ec as u64)
                    .ok_or(SyscallError::NoSuchProcess)
            })
        }
    }
}

fn syscall_detach_thread(handle: u64) -> SyscallResult {
    with_handle_mut(handle, |obj| {
        let thread = map_handle!(obj, Thread, t);
        let mut inner = thread.inner.lock();
        inner.detached = true;
        Ok(0)
    })
}

fn syscall_exit_thread(exit_code: i32) -> SyscallResult {
    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;

    // Update all matching thread handles across all processes (covers duplicated handles).
    let waiters: Vec<process::ProcessId> = {
        let mut found_waiters: Vec<process::ProcessId> = Vec::new();
        process::PROCESS_MANAGER.with_list(|list| {
            for (_, proc) in list.iter_mut() {
                let mut ht = proc.resources.handle_table.lock();
                for (_h, obj) in ht.entries.iter_mut() {
                    if let KernelObject::Thread(t) = obj {
                        let mut inner = t.inner.lock();
                        if inner.pid == pid {
                            inner.exit_code = Some(exit_code);
                            let mut taken = core::mem::take(&mut inner.waiters);
                            found_waiters.append(&mut taken);
                        }
                    }
                }
            }
        });
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

fn syscall_create_window(x: i32, y: i32, width: u32, height: u32, _flags: u64) -> SyscallResult {
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return Err(SyscallError::InvalidArgument);
    }

    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;

    let win_id = kernel::with_kernel_mut(|k| {
        let win_id = k.window.next_window_id();
        let win = crate::contexts::window::Window::new(win_id, "New Window", x, y, width, height);
        k.window.add_window(win);
        win_id
    })
    .ok_or(SyscallError::OutOfMemory)?;

    let state = WindowState {
        window_id: win_id,
        pid,
    };
    Ok(alloc_handle(KernelObject::Window(state))?)
}

fn syscall_destroy_window(handle: u64) -> SyscallResult {
    with_handle_mut(handle, |obj| {
        let id = map_handle!(obj, Window, w).window_id;
        kernel::with_kernel_mut(|k| {
            if let Some(win) = k.window.windows.iter_mut().find(|w| w.id == id) {
                win.visible = false;
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
        let id = map_handle!(obj, Window, w).window_id;
        kernel::with_kernel_mut(|k| {
            if let Some(win) = k.window.windows.iter_mut().find(|w| w.id == id) {
                win.width = width;
                win.height = height;
            }
        });
        Ok(0)
    })
}

fn syscall_present_window(handle: u64) -> SyscallResult {
    with_handle_mut(handle, |obj| {
        let id = map_handle!(obj, Window, w).window_id;
        kernel::with_kernel_mut(|k| {
            if let Some(win) = k.window.windows.iter_mut().find(|w| w.id == id) {
                win.visible = true;
                k.event.push(ResonanceEvent::Window(
                    resonance::event::WindowEvent::Redraw(win.id.0),
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
        let _window = map_handle!(obj, Window, _w);

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

fn syscall_enumerate_devices(class: u64, buf: *mut u8, buf_size: usize) -> SyscallResult {
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

        // Serialize device info: each descriptor is 16 bytes.
        let mut offset = 0;
        for _dev in devices.iter().take(buf_size / 16) {
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
    alloc_handle(KernelObject::Device(DeviceState {}))
}

fn syscall_device_ioctl(handle: u64, _cmd: u64, _arg: u64) -> SyscallResult {
    with_handle_mut(handle, |obj| {
        let _device = map_handle!(obj, Device, _d);
        Err(SyscallError::NotSupported)
    })
}

// ===================================================================
//  IPC syscalls (80–89)
// ===================================================================

fn syscall_channel_create(_flags: u64) -> SyscallResult {
    let inner = alloc::sync::Arc::new(Mutex::new(ChannelInner {
        messages: Vec::with_capacity(16),
        waiters: Vec::new(),
        max_messages: 64,
    }));
    alloc_handle(KernelObject::Channel(ChannelState { inner }))
}

fn syscall_channel_send(handle: u64, data_ptr: *const u8, data_size: u64) -> SyscallResult {
    let size = data_size as usize;
    if size == 0 || size > 65536 {
        return Err(SyscallError::InvalidArgument);
    }

    petroleum::validate_user_buffer(data_ptr as usize, size, false)?;
    let data =
        unsafe { user_slice(data_ptr, size, false) }.map_err(|_| SyscallError::InvalidArgument)?;

    // Allocate the message vector outside the lock to reduce contention.
    let msg_vec = Vec::from(data);

    let recv_waiters: Vec<process::ProcessId> = with_handle_mut(handle, |obj| {
        let channel = map_handle!(obj, Channel, ch);
        let mut inner = channel.inner.lock();
        if inner.messages.len() >= inner.max_messages {
            return Err(SyscallError::Again);
        }
        inner.messages.push(msg_vec);
        Ok(core::mem::take(&mut inner.waiters))
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

    // Dequeue the message inside the lock, then copy to user space
    // outside the lock to avoid holding the spinlock during a
    // potential page fault.
    let msg: Option<Vec<u8>> = with_handle_mut(handle, |obj| {
        let channel = map_handle!(obj, Channel, ch);
        let mut inner = channel.inner.lock();
        if !inner.messages.is_empty() {
            Ok(Some(inner.messages.remove(0)))
        } else {
            Ok(None)
        }
    })?;

    if let Some(msg) = msg {
        let copy_len = msg.len().min(max);
        let dest = unsafe { user_slice_mut(buf, max, false) }
            .map_err(|_| SyscallError::InvalidArgument)?;
        dest[..copy_len].copy_from_slice(&msg[..copy_len]);
        Ok(copy_len as u64)
    } else {
        // No message available — return WouldBlock.
        // NOTE: ChannelState::waiters exists for future blocking-recv
        // support (where the caller would be added to waiters and blocked).
        Err(SyscallError::WouldBlock)
    }
}

fn syscall_pipe_create(_flags: u64) -> SyscallResult {
    // Create a shared buffer for both pipe ends.
    let shared_buffer = alloc::sync::Arc::new(Mutex::new(Vec::with_capacity(4096)));

    let read_end = PipeState {
        buffer: alloc::sync::Arc::clone(&shared_buffer),
        is_read_end: true,
    };
    let write_end = PipeState {
        buffer: shared_buffer,
        is_read_end: false,
    };

    let read_h = alloc_handle(KernelObject::Pipe(read_end))?;
    let write_h = alloc_handle(KernelObject::Pipe(write_end))?;

    // Returns two handles packed into a single u64.
    //
    // NOTE: This assumes both handles fit within 32 bits.  If the system
    // runs long enough or allocates handles rapidly and a handle exceeds
    // u32::MAX, it will be truncated, causing subsequent lookups to fail
    // with BadHandle.  A future version should use a user-space buffer
    // (e.g. `buf: *mut [u64; 2]`) to return both handles without
    // truncation risk.
    Ok(read_h | (write_h << 32))
}

// ===================================================================
//  Handle / Capability syscalls (90–99)
// ===================================================================

fn syscall_handle_transfer(target_pid: u64, handle: u64) -> SyscallResult {
    let target = process::ProcessId(target_pid);
    let current_pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;

    // Remove from current process's handle table
    let mut obj = Some(with_current_handle_table(|ht| {
        ht.entries.remove(&handle).ok_or(SyscallError::BadHandle)
    })?);

    // Insert into target process, returning object if collision
    let result: Option<Option<KernelObject>> =
        process::PROCESS_MANAGER.with_process(target, |p| {
            let our_obj = obj.take().unwrap();
            let mut ht = p.resources.handle_table.lock();
            if ht.entries.contains_key(&handle) {
                return Some(our_obj);
            }
            ht.entries.insert(handle, our_obj);
            None
        });

    match result {
        // Target existed and no collision
        Some(None) => Ok(0),
        // Target existed but had collision — put handle back
        Some(Some(returned_obj)) => {
            let _ = process::PROCESS_MANAGER.with_process(current_pid, |p| {
                let mut ht = p.resources.handle_table.lock();
                ht.entries.insert(handle, returned_obj);
            });
            Err(SyscallError::AlreadyExists)
        }
        // Target doesn't exist — put handle back
        None => {
            let returned_obj = obj.take().unwrap();
            let _ = process::PROCESS_MANAGER.with_process(current_pid, |p| {
                let mut ht = p.resources.handle_table.lock();
                ht.entries.insert(handle, returned_obj);
            });
            Err(SyscallError::NoSuchProcess)
        }
    }
}

fn syscall_handle_duplicate(handle: u64) -> SyscallResult {
    // Find the object and create a duplicate.
    let (new_h, new_obj) = with_current_handle_table(|ht| {
        let obj = ht.entries.get(&handle).ok_or(SyscallError::BadHandle)?;
        let new_h = ht.next_handle;
        ht.next_handle = ht.next_handle.checked_add(1).ok_or(SyscallError::OutOfMemory)?;
        let new_obj = match obj {
            KernelObject::Event(e) => KernelObject::Event(EventState {
                inner: alloc::sync::Arc::clone(&e.inner),
            }),
            KernelObject::Thread(t) => KernelObject::Thread(ThreadState {
                inner: alloc::sync::Arc::clone(&t.inner),
            }),
            KernelObject::Channel(ch) => KernelObject::Channel(ChannelState {
                inner: alloc::sync::Arc::clone(&ch.inner),
            }),
            KernelObject::Window(w) => KernelObject::Window(WindowState {
                window_id: w.window_id,
                pid: w.pid,
            }),
            KernelObject::Pipe(p) => KernelObject::Pipe(PipeState {
                buffer: alloc::sync::Arc::clone(&p.buffer),
                is_read_end: p.is_read_end,
            }),
            _ => return Err(SyscallError::NotSupported),
        };
        ht.entries.insert(new_h, new_obj);
        Ok(new_h)
    })?;
    Ok(new_h)
}

fn syscall_handle_revoke(handle: u64) -> SyscallResult {
    with_current_handle_table(|ht| {
        if ht.entries.remove(&handle).is_some() {
            Ok(0)
        } else {
            Err(SyscallError::BadHandle)
        }
    })
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
    check_and_fire_timers();
}

/// Check all timers and fire (signal) any that have expired.
/// Called from tick_uptime during timer interrupts.
pub fn check_and_fire_timers() {
    let now_ns = uptime_us() * 1000; // convert microseconds to nanoseconds

    // Collect expired timers across all processes, outside the handle-table locks.
    let expired: Vec<(u64, u64)> = {
        let mut expired_timers = Vec::new();
        process::PROCESS_MANAGER.with_list(|list| {
            for (_, proc) in list.iter_mut() {
                let mut ht = proc.resources.handle_table.lock();
                for (handle, obj) in ht.entries.iter_mut() {
                    if let KernelObject::Timer(timer) = obj {
                        if !timer.fired && now_ns >= timer.deadline_ns {
                            timer.fired = true;
                            expired_timers.push((*handle, timer.event_handle));
                        }
                    }
                }
            }
        });
        expired_timers
    };

    // Signal the event for each expired timer.
    for (_timer_handle, event_handle) in expired {
        // Collect waiters across all processes, then unblock outside locks
        let waiters_to_unblock: Vec<process::ProcessId> = {
            let mut found = Vec::new();
            process::PROCESS_MANAGER.with_list(|list| {
                for (_, proc) in list.iter_mut() {
                    let mut ht = proc.resources.handle_table.lock();
                    if let Some(KernelObject::Event(e)) = ht.entries.get_mut(&event_handle) {
                        let mut inner = e.inner.lock();
                        inner.signaled = true;
                        found = core::mem::take(&mut inner.waiters);
                    }
                }
            });
            found
        };
        for pid in waiters_to_unblock {
            crate::process::unblock_process(pid);
        }
    }
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
    // Validate the event handle exists in the current process before creating the timer.
    with_current_handle_table(|ht| {
        match ht.entries.get(&event_handle) {
            Some(KernelObject::Event(_)) => Ok(()),
            _ => Err(SyscallError::BadHandle),
        }
    })?;

    let timer = TimerState {
        deadline_ns,
        event_handle,
        fired: false,
    };
    alloc_handle(KernelObject::Timer(timer))
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
        // TODO: Register with a timer queue so the process is set to
        // Blocked and unblocked on expiry.
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
    let data =
        unsafe { user_slice_mut(buf, 8, false) }.map_err(|_| SyscallError::InvalidArgument)?;
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
