use alloc::boxed::Box;
use alloc::vec;
use core::ffi::c_int;

use petroleum::common::memory::UserSlice;
use petroleum::page_table::PageTableHelper;
use x86_64::PhysAddr;

use super::interface::{SyscallError, SyscallResult, copy_user_string};
use super::process::{alloc_kernel_stack, free_kernel_stack, with_current_fd_table};
use crate::linux::{O_APPEND, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY};
use crate::process::{self, Process, ProcessState};

pub(crate) fn syscall_abi_version() -> SyscallResult {
    let ver = fullerene_abi::AbiVersion::CURRENT;
    let packed = (ver.major as u64) << 48
        | (ver.minor as u64) << 32
        | (ver.patch as u64) << 16
        | ver.reserved as u64;
    Ok(packed)
}

pub(crate) fn syscall_exit(exit_code: i32) -> SyscallResult {
    let pid = process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    process::terminate_process(pid, exit_code);
    Ok(0)
}

pub(crate) fn syscall_fork() -> SyscallResult {
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
        let alloc = unsafe { petroleum::page_table::constants::get_frame_allocator_mut() };
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
        .map_err(|_| {
            crate::memory_management::deallocate_process_page_table(cloned_pml4_frame);
            SyscallError::InvalidArgument
        })?;

    let (kernel_stack_ptr, kernel_stack_top) = alloc_kernel_stack().map_err(|e| {
        crate::memory_management::deallocate_process_page_table(cloned_pml4_frame);
        e
    })?;

    let child_pid = process::PROCESS_MANAGER.allocate_pid().0 as usize;

    let _ = child_page_table.unmap_page(petroleum::vdso::VDSO_USER_BASE as usize);

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

pub(crate) fn syscall_read(fd: c_int, buffer: *mut u8, count: usize) -> SyscallResult {
    let count = count.min(65536);
    if count == 0 {
        return Ok(0);
    }

    let slice = UserSlice::new(buffer, count, true)
        .map_err(|_| SyscallError::InvalidArgument)?;

    petroleum::validate_syscall_fd(fd)?;

    if fd == 0 {
        if count == 1 {
            if let Some(ch) = nitrogen::ps2::keyboard::read_char() {
                let kernel_buf = [ch];
                unsafe { slice.copy_to_user(&kernel_buf) }
                    .map_err(|_| SyscallError::InvalidArgument)?;
                Ok(1)
            } else {
                Ok(0)
            }
        } else {
            let mut kernel_buf = vec![0u8; count];
            let bytes_read = nitrogen::ps2::keyboard::drain_line_buffer(&mut kernel_buf);
            unsafe { slice.copy_to_user(&kernel_buf[..bytes_read]) }
                .map_err(|_| SyscallError::InvalidArgument)?;
            Ok(bytes_read as u64)
        }
    } else {
        if fd < 0 {
            return Err(SyscallError::BadFileDescriptor);
        }
        with_current_fd_table(|ft| {
            match ft.entries.get_mut(&(fd as u32)) {
                Some(file_desc) => {
                    let mut kernel_buf = vec![0u8; count];
                    match crate::fs::read_file(file_desc, &mut kernel_buf) {
                        Ok(n) => {
                            unsafe { slice.copy_to_user(&kernel_buf[..n]) }
                                .map_err(|_| SyscallError::InvalidArgument)?;
                            Ok(n as u64)
                        }
                        Err(_) => Err(SyscallError::BadFileDescriptor),
                    }
                }
                None => Err(SyscallError::BadFileDescriptor),
            }
        })
    }
}

pub(crate) fn syscall_write(fd: c_int, buffer: *const u8, count: usize) -> SyscallResult {
    petroleum::validate_syscall_fd(fd)?;
    let count = count.min(65536);
    if count == 0 {
        return Ok(0);
    }

    let slice = UserSlice::new(buffer as *mut u8, count, false)
        .map_err(|_| SyscallError::InvalidArgument)?;

    let mut kernel_buf = vec![0u8; count];
    unsafe { slice.copy_from_user(&mut kernel_buf) }
        .map_err(|_| SyscallError::InvalidArgument)?;

    if fd == 1 || fd == 2 {
        petroleum::write_serial_bytes(0x3F8, 0x3FD, &kernel_buf);
        Ok(count as u64)
    } else {
        Err(SyscallError::BadFileDescriptor)
    }
}

pub(crate) fn syscall_open(filename: *const u8, flags: c_int, _mode: u32) -> SyscallResult {
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

pub(crate) fn syscall_close(fd: c_int) -> SyscallResult {
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

pub(crate) fn syscall_wait(pid: u64) -> SyscallResult {
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
            // Re-read exit_code after unblock (parent was unblocked by terminate_process)
            let ec = crate::process::PROCESS_MANAGER
                .with_process(pid_type, |process| process.exit_code)
                .flatten()
                .unwrap_or(0);
            Ok(ec as u64)
        } else {
            Err(SyscallError::NoSuchProcess)
        }
    }
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

    crate::process::PROCESS_MANAGER
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
