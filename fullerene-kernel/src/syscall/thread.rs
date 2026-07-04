use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::VirtAddr;

use crate::map_handle;

use super::interface::{SyscallError, SyscallResult};
use super::process::{alloc_handle, alloc_kernel_stack, free_kernel_stack, with_handle_mut};
use super::types::*;
use crate::process::{self, Process, ProcessState};

pub(crate) fn syscall_create_thread(entry: u64, stack: u64, _flags: u64) -> SyscallResult {
    let entry_point = VirtAddr::try_new(entry)
        .map_err(|_| SyscallError::InvalidArgument)?;
    let user_stack = VirtAddr::try_new(stack)
        .map_err(|_| SyscallError::InvalidArgument)?;

    if !petroleum::is_user_address(entry_point) {
        return Err(SyscallError::InvalidArgument);
    }

    if !petroleum::is_user_address(user_stack) {
        return Err(SyscallError::InvalidArgument);
    }

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
        page_table: None,
        kernel_stack: kernel_stack_top,
        user_stack,
        entry_point,
        is_user: true,
        task_data: 0,
        exit_code: None,
        parent_id: Some(current_pid),
        dispatch_mode: None,
        vdso_page: None,
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

    let inner = Arc::new(Mutex::new(ThreadInner {
        pid: child_pid,
        detached: false,
        exit_code: None,
        waiters: Vec::new(),
    }));
    let handle = alloc_handle(KernelObject::Thread(ThreadState { inner }));
    if handle.is_err() {
        // Clean up: remove thread from process manager and free kernel stack
        crate::process::PROCESS_MANAGER.with_list(|list| {
            if let Some(pos) = list.iter().position(|(id, _)| *id == child_pid) {
                let _ = list.swap_remove(pos);
            }
        });
        free_kernel_stack(kernel_stack_ptr);
    }
    handle
}

pub(crate) fn syscall_join_thread(handle: u64) -> SyscallResult {
    let h = Handle::from_raw(handle);
    let done = with_handle_mut(h, |obj| {
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
            with_handle_mut(h, |obj| {
                let thread = map_handle!(obj, Thread, t);
                let inner = thread.inner.lock();
                if let Some(exit_code) = inner.exit_code {
                    Ok(exit_code as u64)
                } else {
                    // Detect lost-wakeup race: exit_thread consumed our PID
                    // from waiters before block_current() completed.
                    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
                    if !inner.waiters.contains(&pid) {
                        // exit_code was written then consumed — re-read
                        inner.exit_code.map(|ec| ec as u64)
                            .ok_or(SyscallError::NoSuchProcess)
                    } else {
                        Err(SyscallError::NoSuchProcess)
                    }
                }
            })
        }
    }
}

pub(crate) fn syscall_detach_thread(handle: u64) -> SyscallResult {
    let h = Handle::from_raw(handle);
    with_handle_mut(h, |obj| {
        let thread = map_handle!(obj, Thread, t);
        let mut inner = thread.inner.lock();
        inner.detached = true;
        Ok(0)
    })
}

pub(crate) fn syscall_exit_thread(exit_code: i32) -> SyscallResult {
    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;

    let waiters: Vec<process::ProcessId> = {
        let mut found_waiters: Vec<process::ProcessId> = Vec::new();
        process::PROCESS_MANAGER.with_list(|list| {
            for (_, proc) in list.iter_mut() {
                let mut ht = proc.resources.handle_table.lock();
                for obj in ht.iter_objects_mut() {
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
