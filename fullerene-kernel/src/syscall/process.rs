//! Native process lifecycle syscalls and per-process resource access.

use alloc::boxed::Box;
use alloc::vec;
use core::alloc::Layout;

use petroleum::common::memory::UserSlice;
use petroleum::page_table::PageTableHelper;
use x86_64::{PhysAddr, VirtAddr};

use super::interface::{SyscallError, SyscallResult};
use super::types::{Handle, HandlePerms, KernelObject};
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

    let (parent_page_table_phys_addr, parent_context, parent_user_stack, parent_entry_point) = {
        process::SCHEDULER
            .with_process(current_pid, |process| {
                (
                    process.page_table_phys_addr,
                    process.context.clone(),
                    process.user_stack,
                    process.entry_point,
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
        let mut allocator_guard = crate::heap::FRAME_ALLOCATOR.lock();
        let allocator = match allocator_guard.as_mut() {
            Some(allocator) => allocator,
            None => {
                drop(allocator_guard);
                free_kernel_stack(kernel_stack_ptr);
                crate::memory_management::deallocate_process_page_table(cloned_pml4_frame);
                return Err(SyscallError::OutOfMemory);
            }
        };
        let vdso =
            crate::vdso::create_vdso_page(&mut child_page_table, allocator, child_pid as u64);
        drop(allocator_guard);
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

    process::SCHEDULER
        .add(Box::new(child_process))
        .map_err(|_| {
            free_kernel_stack(kernel_stack_ptr);
            crate::memory_management::deallocate_process_page_table(cloned_pml4_frame);
            SyscallError::OutOfMemory
        })?;

    Ok(child_pid as u64)
}

pub(crate) fn syscall_wait(pid: u64) -> SyscallResult {
    if pid == 0 {
        process::yield_current();
        return Ok(0);
    }

    let process_id = process::ProcessId(pid);
    let result = process::SCHEDULER
        .with_process(process_id, |process| {
            if process.state == ProcessState::Terminated {
                Some(process.exit_code.unwrap_or(0))
            } else {
                None
            }
        })
        .flatten();

    if let Some(exit_code) = result {
        return Ok(exit_code as u64);
    }
    if process::SCHEDULER
        .with_process(process_id, |_| {})
        .is_none()
    {
        return Err(SyscallError::NoSuchProcess);
    }

    process::block_current();
    let exit_code = process::SCHEDULER
        .with_process(process_id, |process| process.exit_code)
        .flatten()
        .unwrap_or(0);
    Ok(exit_code as u64)
}

pub(crate) fn syscall_getpid() -> SyscallResult {
    Ok(process::current_pid().map(|pid| pid.0).unwrap_or(0))
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
