//! Native process lifecycle syscalls and per-process resource access.

use alloc::boxed::Box;
use alloc::vec;
use core::alloc::Layout;

use petroleum::common::memory::UserSlice;
use petroleum::page_table::PageTableHelper;
use x86_64::{PhysAddr, VirtAddr};

use super::interface::{SyscallError, SyscallResult};
use super::types::{Handle, HandlePerms, KernelObject, ProcessControlState};
use crate::process::{self, Process, ProcessState};

pub(crate) fn with_current_fd_table<F, R>(f: F) -> Result<R, SyscallError>
where
    F: FnOnce(&mut process::FdTable) -> Result<R, SyscallError>,
{
    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    match process::SCHEDULER.with_process(pid, |p| {
        let mut ft = p.resources.fd_table.lock();
        f(&mut *ft)
    }) {
        Some(r) => r,
        None => Err(SyscallError::NoSuchProcess),
    }
}

pub(crate) fn with_current_handle_table<F, R>(f: F) -> Result<R, SyscallError>
where
    F: FnOnce(&mut crate::process::HandleTable) -> Result<R, SyscallError>,
{
    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    match process::SCHEDULER.with_process(pid, |p| {
        let mut ht = p.resources.handle_table.lock();
        f(&mut *ht)
    }) {
        Some(r) => r,
        None => Err(SyscallError::NoSuchProcess),
    }
}

pub(crate) fn with_kernel_mut_result<F>(f: F) -> SyscallResult
where
    F: FnOnce(&mut crate::contexts::KernelContext) -> SyscallResult,
{
    crate::contexts::kernel::with_kernel_mut(f).ok_or(SyscallError::NotSupported)?
}

pub(crate) fn alloc_handle(obj: KernelObject) -> Result<u64, SyscallError> {
    with_current_handle_table(|ht| {
        let h = ht.alloc(obj);
        Ok(h.raw())
    })
}

pub(crate) fn with_handle_mut<F, R>(h: Handle, f: F) -> Result<R, SyscallError>
where
    F: FnOnce(&mut KernelObject) -> Result<R, SyscallError>,
{
    with_current_handle_table(|ht| match ht.get_mut(h) {
        Some(obj) => f(obj),
        None => Err(SyscallError::BadHandle),
    })
}

pub(crate) fn with_handle<F, R>(h: Handle, f: F) -> Result<R, SyscallError>
where
    F: FnOnce(&KernelObject) -> Result<R, SyscallError>,
{
    with_current_handle_table(|ht| match ht.get(h) {
        Some(obj) => f(obj),
        None => Err(SyscallError::BadHandle),
    })
}

pub(crate) fn check_handle_permission(
    h: Handle,
    required: HandlePerms,
) -> Result<(), SyscallError> {
    with_current_handle_table(|ht| {
        if !ht.check_perm(h, required) {
            Err(SyscallError::PermissionDenied)
        } else {
            Ok(())
        }
    })
}

pub(crate) fn alloc_kernel_stack() -> Result<(*mut u8, VirtAddr), SyscallError> {
    let layout = Layout::from_size_align(crate::heap::KERNEL_STACK_SIZE, 16).unwrap();
    let ptr = petroleum::common::memory::allocate_layout(layout)
        .map_err(|_| SyscallError::OutOfMemory)?;
    let top = VirtAddr::new(ptr as u64 + crate::heap::KERNEL_STACK_SIZE as u64);
    Ok((ptr, top))
}

pub(crate) fn free_kernel_stack(ptr: *mut u8) {
    let layout = Layout::from_size_align(crate::heap::KERNEL_STACK_SIZE, 16).unwrap();
    unsafe { petroleum::common::memory::deallocate_layout(ptr, layout) };
}

pub(crate) fn syscall_exit(exit_code: i32) -> SyscallResult {
    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    process::terminate_process(pid, exit_code);
    Ok(0)
}

pub(crate) fn syscall_fork() -> SyscallResult {
    let current_pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;

    let (
        parent_page_table_phys_addr,
        parent_context,
        parent_fpu,
        parent_user_stack,
        parent_entry_point,
        parent_terminal_id,
    ) = {
        process::SCHEDULER
            .with_process(current_pid, |process| {
                (
                    process.page_table_phys_addr,
                    process.context.clone(),
                    crate::fpu::save_and_snapshot(process.fpu_state.as_mut()),
                    process.user_stack,
                    process.entry_point,
                    process.terminal_id,
                )
            })
            .ok_or(SyscallError::NoSuchProcess)?
    };

    let cloned_table_addr = {
        let mut manager_guard = crate::memory_management::get_memory_manager().lock();
        let manager = manager_guard.as_mut().ok_or(SyscallError::OutOfMemory)?;

        let page_table_manager = &mut manager.page_table_manager;
        petroleum::page_table::constants::with_frame_allocator(|allocator| {
            PageTableHelper::clone_page_table(
                page_table_manager,
                parent_page_table_phys_addr.as_u64() as usize,
                allocator,
            )
        })?
    };

    let cloned_pml4_frame = x86_64::structures::paging::PhysFrame::containing_address(
        PhysAddr::new(cloned_table_addr as u64),
    );

    let mut child_page_table =
        petroleum::page_table::ProcessPageTable::new_with_frame(cloned_pml4_frame);
    petroleum::initializer::Initializable::init(&mut child_page_table).map_err(|_| {
        crate::memory_management::deallocate_process_page_table(cloned_pml4_frame);
        SyscallError::InvalidArgument
    })?;

    let (kernel_stack_ptr, kernel_stack_top) = alloc_kernel_stack().map_err(|error| {
        crate::memory_management::deallocate_process_page_table(cloned_pml4_frame);
        error
    })?;

    let child_pid = process::SCHEDULER.allocate_pid().0 as usize;
    let _ = child_page_table.unmap_page(petroleum::vdso::VDSO_USER_BASE as usize);

    let child_vdso = if parent_context.is_user {
        let vdso = petroleum::page_table::constants::with_frame_allocator(|frame_allocator| {
            crate::vdso::create_vdso_page(&mut child_page_table, frame_allocator, child_pid as u64)
        });
        match vdso {
            Ok(vdso) => Some(vdso),
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
        name: Box::from("child"),
        state: ProcessState::Ready,
        context: parent_context.clone(),
        fpu_state: Box::new(parent_fpu),
        page_table_phys_addr: PhysAddr::new(cloned_table_addr as u64),
        page_table: Some(Box::new(child_page_table)),
        kernel_stack: kernel_stack_top,
        user_stack: parent_user_stack,
        entry_point: parent_entry_point,
        is_user: parent_context.is_user,
        role: if parent_context.is_user {
            crate::process::ProcessRole::User
        } else {
            crate::process::ProcessRole::Kernel
        },
        task_data: 0,
        exit_code: None,
        fault: None,
        parent_id: Some(current_pid),
        supervisor_id: Some(current_pid),
        reaped: false,
        terminal_id: parent_terminal_id,
        terminal_owner: false,
        nozzle_authorized: false,
        dispatch_mode: None,
        syscall_state: None,
        vdso_page: child_vdso,
        resources: process::ProcessResources::new(),
    };

    child_process.context.registers.rax = 0;
    child_process.context.registers.rsp = child_process.user_stack.as_u64();
    child_process.context.kernel_rsp = 0;

    process::SCHEDULER
        .add(Box::new(child_process))
        .map_err(|_| SyscallError::OutOfMemory)?;

    Ok(child_pid as u64)
}

pub(crate) fn syscall_wait(pid: u64) -> SyscallResult {
    if pid == 0 {
        process::yield_current();
        return Ok(0);
    }

    let current_pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    let wait_any = pid == u64::MAX;
    let process_id = process::ProcessId(pid);
    if !wait_any {
        let is_child = process::SCHEDULER
            .with_process(process_id, |child| child.parent_id == Some(current_pid))
            .ok_or(SyscallError::NoSuchProcess)?;
        if !is_child {
            return Err(SyscallError::PermissionDenied);
        }
    }
    loop {
        let state = if wait_any {
            process::SCHEDULER.with_list(|list| {
                list.iter()
                    .find(|(_, process)| {
                        process.parent_id == Some(current_pid)
                            && process.state == ProcessState::Terminated
                    })
                    .map(|(_, process)| (process.id, process.state, process.exit_code))
            })
        } else {
            process::SCHEDULER.with_process(process_id, |process| {
                (process.id, process.state, process.exit_code)
            })
        };

        match state {
            Some((waited_pid, ProcessState::Terminated, exit_code)) => {
                let _ = process::SCHEDULER.with_process(waited_pid, |process| {
                    process.reaped = true;
                });
                return Ok(encode_exit_code(exit_code.unwrap_or(0)));
            }
            Some(_) => {
                // A sibling child can also wake this parent. Re-check the
                // requested child after every wakeup instead of treating any
                // parent wakeup as completion of this wait.
                process::block_current();
            }
            None if wait_any => {
                let has_child = process::SCHEDULER.with_list(|list| {
                    list.iter()
                        .any(|(_, process)| process.parent_id == Some(current_pid))
                });
                if has_child {
                    process::block_current();
                } else {
                    return Err(SyscallError::NoSuchProcess);
                }
            }
            None => return Err(SyscallError::NoSuchProcess),
        }
    }
}

/// Exit codes are data, not syscall errors. Encode the signed i32 in the
/// low 32 bits so a child returning (for example) `-1` cannot be mistaken for
/// a negative kernel errno by user-space syscall wrappers.
fn encode_exit_code(exit_code: i32) -> u64 {
    exit_code as u32 as u64
}

/// Open a capability for supervising a process. The caller must be its
/// parent or its recorded supervisor; the returned handle can then be
/// transferred to an independent manager process.
pub(crate) fn syscall_open_process_control(pid: u64) -> SyscallResult {
    let target = process::ProcessId(pid);
    let caller = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    let allowed = process::SCHEDULER
        .with_process(target, |child| {
            child.parent_id == Some(caller) || child.supervisor_id == Some(caller)
        })
        .ok_or(SyscallError::NoSuchProcess)?;
    if !allowed {
        return Err(SyscallError::PermissionDenied);
    }
    alloc_handle(KernelObject::ProcessControl(ProcessControlState {
        pid: target,
    }))
}

fn process_control_target(handle: u64) -> Result<process::ProcessId, SyscallError> {
    let handle = Handle::from_raw(handle);
    check_handle_permission(handle, HandlePerms::SIGNAL)?;
    with_current_handle_table(|table| match table.get(handle) {
        Some(KernelObject::ProcessControl(control)) => Ok(control.pid),
        Some(_) => Err(SyscallError::BadHandle),
        None => Err(SyscallError::BadHandle),
    })
}

pub(crate) fn syscall_process_control_stop(handle: u64, exit_code: i32) -> SyscallResult {
    let target = process_control_target(handle)?;
    let is_init = process::SCHEDULER
        .with_process(target, |process| process.role == process::ProcessRole::Init)
        .ok_or(SyscallError::NoSuchProcess)?;
    if is_init {
        return Err(SyscallError::PermissionDenied);
    }
    process::terminate_process(target, exit_code);
    Ok(0)
}

pub(crate) fn syscall_process_control_status(handle: u64) -> SyscallResult {
    let target = process_control_target(handle)?;
    let state = process::SCHEDULER
        .with_process(target, |process| match process.state {
            ProcessState::Ready => 0,
            ProcessState::Running => 1,
            ProcessState::Blocked => 2,
            ProcessState::Terminated => 3,
        })
        .ok_or(SyscallError::NoSuchProcess)?;
    Ok(state)
}

pub(crate) fn syscall_process_control_reap(handle: u64) -> SyscallResult {
    let target = process_control_target(handle)?;
    let exit_code = process::SCHEDULER
        .with_process(target, |process| {
            if process.state != ProcessState::Terminated {
                return None;
            }
            let code = process.exit_code;
            if code.is_some() {
                process.reaped = true;
            }
            code
        })
        .ok_or(SyscallError::NoSuchProcess)?
        .ok_or(SyscallError::WouldBlock)?;
    Ok(encode_exit_code(exit_code))
}

/// Change the designated supervisor without changing the birth parent. The
/// caller must hold the process-control capability; the manager can then be
/// given the same handle using the ordinary capability-transfer syscall.
pub(crate) fn syscall_process_control_assign(handle: u64, supervisor_pid: u64) -> SyscallResult {
    let target = process_control_target(handle)?;
    let supervisor = process::ProcessId(supervisor_pid);
    let valid_supervisor = process::SCHEDULER
        .with_process(supervisor, |process| {
            process.state != ProcessState::Terminated && process.role != process::ProcessRole::Idle
        })
        .ok_or(SyscallError::NoSuchProcess)?;
    if !valid_supervisor {
        return Err(SyscallError::NoSuchProcess);
    }
    process::SCHEDULER
        .with_process(target, |process| process.supervisor_id = Some(supervisor))
        .ok_or(SyscallError::NoSuchProcess)?;
    Ok(0)
}

pub(crate) fn syscall_getpid() -> SyscallResult {
    Ok(process::current_pid().map(|pid| pid.0).unwrap_or(0))
}

/// Poll the kernel-to-launchd desktop request queue. This is intentionally
/// restricted to PID 1: ordinary applications request a shell through the
/// desktop callback and cannot consume or forge launchd control messages.
pub(crate) fn syscall_launchd_poll_request() -> SyscallResult {
    let caller = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    let is_init = process::SCHEDULER
        .with_process(caller, |current| current.role == process::ProcessRole::Init)
        .ok_or(SyscallError::NoSuchProcess)?;
    if !is_init {
        return Err(SyscallError::PermissionDenied);
    }
    Ok(crate::scheduler::take_shell_launch_request() as u64)
}

/// Enter the compatibility Nozzle runtime for the launchd-owned shell image.
///
/// The shell image is still a normal native user ELF and remains supervised by
/// PID 1.  Nozzle currently depends on the kernel's VFS and desktop service
/// callbacks, so this narrow ABI bridge runs it on the calling process's
/// terminal until the user exits.  Restricting it to the launchd shell image
/// keeps the privileged kernel command surface out of arbitrary user ELFs.
pub(crate) fn syscall_run_nozzle() -> SyscallResult {
    let caller = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    let authorized = process::SCHEDULER
        .with_process(caller, |current| {
            current.nozzle_authorized && current.terminal_id.is_some()
        })
        .ok_or(SyscallError::NoSuchProcess)?;
    if !authorized {
        return Err(SyscallError::PermissionDenied);
    }

    crate::shell::shell_main_on_current_terminal();
    Ok(0)
}

pub(crate) fn syscall_get_process_name(buffer: *mut u8, size: usize) -> SyscallResult {
    if size == 0 {
        return Err(SyscallError::InvalidArgument);
    }
    petroleum::validate_user_buffer(buffer as usize, size, false)?;
    let current_pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;

    process::SCHEDULER
        .with_process(current_pid, |process| {
            let name_bytes = process.name.as_bytes();
            let copy_len = name_bytes.len().min(size - 1);

            let mut kernel_buf = vec![0u8; copy_len + 1];
            kernel_buf[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
            kernel_buf[copy_len] = b'\0';

            let slice = UserSlice::new(buffer, copy_len + 1, true)
                .map_err(|_| SyscallError::InvalidArgument)?;
            unsafe { slice.copy_to_user(&kernel_buf) }
                .map_err(|_| SyscallError::InvalidArgument)?;
            Ok(copy_len as u64)
        })
        .ok_or(SyscallError::NoSuchProcess)?
}

pub(crate) fn syscall_yield() -> SyscallResult {
    process::yield_current();
    Ok(0)
}

const MAX_EXECUTABLE_BYTES: usize = 64 * 1024 * 1024;
const MAX_PROCESS_NAME_BYTES: usize = 64;

/// Copy an ELF image from the caller and start it in a new isolated process.
pub(crate) fn syscall_spawn(
    image_ptr: *const u8,
    image_len: usize,
    name_ptr: *const u8,
    name_len: usize,
    terminal_id: u64,
    supervisor_pid: u64,
) -> SyscallResult {
    if image_len == 0
        || image_len > MAX_EXECUTABLE_BYTES
        || name_len == 0
        || name_len > MAX_PROCESS_NAME_BYTES
    {
        return Err(SyscallError::InvalidArgument);
    }

    let image_slice = UserSlice::new(image_ptr as *mut u8, image_len, false)
        .map_err(|_| SyscallError::AddressFault)?;
    let name_slice = UserSlice::new(name_ptr as *mut u8, name_len, false)
        .map_err(|_| SyscallError::AddressFault)?;
    let mut image = vec![0u8; image_len];
    let mut name_bytes = vec![0u8; name_len];
    unsafe {
        image_slice
            .copy_from_user(&mut image)
            .map_err(|_| SyscallError::AddressFault)?;
        name_slice
            .copy_from_user(&mut name_bytes)
            .map_err(|_| SyscallError::AddressFault)?;
    }
    let name = core::str::from_utf8(&name_bytes)
        .map_err(|_| SyscallError::InvalidArgument)?
        .trim();
    if name.is_empty()
        || name
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(SyscallError::InvalidArgument);
    }

    let parent_id = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    let caller_is_init = process::SCHEDULER
        .with_process(parent_id, |current| {
            current.role == process::ProcessRole::Init
        })
        .ok_or(SyscallError::NoSuchProcess)?;
    if terminal_id != 0 && !process::SCHEDULER.terminal_owned_by(terminal_id, parent_id) {
        return Err(SyscallError::PermissionDenied);
    }
    let supervisor_id = if supervisor_pid == 0 {
        Some(parent_id)
    } else {
        let supervisor = process::ProcessId(supervisor_pid);
        let valid_supervisor = process::SCHEDULER
            .with_process(supervisor, |process| {
                process.state != ProcessState::Terminated
                    && process.role != process::ProcessRole::Idle
            })
            .ok_or(SyscallError::NoSuchProcess)?;
        if !valid_supervisor {
            return Err(SyscallError::NoSuchProcess);
        }
        Some(supervisor)
    };
    let result = crate::loader::load_program_with_relationships_and_authorization(
        &image,
        name,
        parent_id,
        supervisor_id,
        (terminal_id != 0).then_some(terminal_id),
        caller_is_init,
    )
    .map(|pid| pid.0)
    .map_err(|error| match error {
        crate::loader::LoadError::OutOfMemory => SyscallError::OutOfMemory,
        crate::loader::LoadError::FileNotFound => SyscallError::FileNotFound,
        crate::loader::LoadError::InvalidFormat
        | crate::loader::LoadError::NotExecutable
        | crate::loader::LoadError::UnsupportedArchitecture => SyscallError::InvalidArgument,
        crate::loader::LoadError::MappingFailed
        | crate::loader::LoadError::AddressAlreadyMapped => SyscallError::Io,
    });
    match result {
        Ok(pid) => {
            if terminal_id != 0
                && !process::SCHEDULER.transfer_terminal_owner(
                    terminal_id,
                    parent_id,
                    process::ProcessId(pid),
                )
            {
                process::terminate_process(process::ProcessId(pid), -1);
                return Err(SyscallError::PermissionDenied);
            }
            Ok(pid)
        }
        Err(error) => {
            if terminal_id != 0 {
                process::SCHEDULER.close_terminal_if_owned(terminal_id, parent_id);
            }
            Err(error)
        }
    }
}
