use fullerene_abi::syscall_numbers::*;

use super::basic;
use super::cap;
use super::device;
use super::event;
use super::interface::SyscallError;
use super::ipc;
use super::memory;
use super::thread;
use super::time;
use super::window;

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
    let current_pid = crate::process::current_pid();
    let dispatch_mode = current_pid
        .and_then(|pid| {
            crate::process::SCHEDULER.with_process(pid, |p| {
                matches!(p.dispatch_mode, Some(crate::linux::DispatchMode::Linux(_)))
            })
        })
        .unwrap_or(false);

    if dispatch_mode {
        let mut linux_rt = current_pid.and_then(|pid| {
            crate::process::SCHEDULER
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
                crate::process::SCHEDULER.with_process(pid, |p| {
                    p.dispatch_mode = Some(crate::linux::DispatchMode::Linux(rt));
                });
            }
            result
        } else {
            crate::linux::errno_code(crate::linux::ENOSYS)
        };
        return ret;
    }

    let result = match syscall_num {
        ABI_VERSION => basic::syscall_abi_version(),

        EXIT => basic::syscall_exit(arg1 as i32),
        FORK => basic::syscall_fork(),
        READ => basic::syscall_read(arg1 as core::ffi::c_int, arg2 as *mut u8, arg3 as usize),
        WRITE => basic::syscall_write(arg1 as core::ffi::c_int, arg2 as *const u8, arg3 as usize),
        OPEN => basic::syscall_open(arg1 as *const u8, arg2 as core::ffi::c_int, arg3 as u32),
        CLOSE => basic::syscall_close(arg1 as core::ffi::c_int),
        WAIT => basic::syscall_wait(arg1 as u64),
        GETPID => basic::syscall_getpid(),
        GET_PROCESS_NAME => basic::syscall_get_process_name(arg1 as *mut u8, arg2 as usize),
        YIELD => basic::syscall_yield(),

        MAP_MEMORY => memory::syscall_map_memory(arg1, arg2, arg3),
        UNMAP_MEMORY => memory::syscall_unmap_memory(arg1, arg2),
        PROTECT_MEMORY => memory::syscall_protect_memory(arg1, arg2, arg3),
        QUERY_MEMORY => memory::syscall_query_memory(arg1 as *mut u8, arg2 as usize),

        CREATE_EVENT => event::syscall_create_event(arg1),
        WAIT_EVENT => event::syscall_wait_event(arg1, arg2),
        SIGNAL_EVENT => event::syscall_signal_event(arg1),
        SUBSCRIBE_EVENT => event::syscall_subscribe_event(arg1, arg2),

        CREATE_THREAD => thread::syscall_create_thread(arg1, arg2, arg3),
        JOIN_THREAD => thread::syscall_join_thread(arg1),
        DETACH_THREAD => thread::syscall_detach_thread(arg1),
        EXIT_THREAD => thread::syscall_exit_thread(arg1 as i32),

        CREATE_WINDOW => window::syscall_create_window(arg1 as i32, arg2 as i32, arg3 as u32, arg4 as u32, arg5),
        DESTROY_WINDOW => window::syscall_destroy_window(arg1),
        RESIZE_WINDOW => window::syscall_resize_window(arg1, arg2 as u32, arg3 as u32),
        PRESENT_WINDOW => window::syscall_present_window(arg1),
        GET_WINDOW_EVENT => window::syscall_get_window_event(arg1, arg2 as *mut u8, arg3 as usize),

        ENUMERATE_DEVICES => device::syscall_enumerate_devices(arg1, arg2 as *mut u8, arg3 as usize),
        OPEN_DEVICE => device::syscall_open_device(arg1 as *const u8),
        DEVICE_IOCTL => device::syscall_device_ioctl(arg1, arg2, arg3),

        CHANNEL_CREATE => ipc::syscall_channel_create(arg1),
        CHANNEL_SEND => ipc::syscall_channel_send(arg1, arg2 as *const u8, arg3),
        CHANNEL_RECV => ipc::syscall_channel_recv(arg1, arg2 as *mut u8, arg3),
        PIPE_CREATE => ipc::syscall_pipe_create(arg1 as *mut u64),

        HANDLE_TRANSFER => cap::syscall_handle_transfer(arg1 as u64, arg2),
        HANDLE_DUPLICATE => cap::syscall_handle_duplicate(arg1),
        HANDLE_REVOKE => cap::syscall_handle_revoke(arg1),

        CLOCK_GETTIME => time::syscall_clock_gettime(arg1, arg2 as *mut u8),
        TIMER_CREATE => time::syscall_timer_create(arg1, arg2, arg3),
        SLEEP => time::syscall_sleep(arg1),
        UPTIME => time::syscall_uptime(arg1 as *mut u8),

        _ => Err(SyscallError::InvalidSyscall),
    };

    match result {
        Ok(value) => value,
        Err(error) => -(error as i32) as u64,
    }
}

pub fn kernel_syscall(syscall_num: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    unsafe { handle_syscall(syscall_num, arg1, arg2, arg3, 0, 0, 0) }
}

pub fn init() {
    use crate::interrupts::syscall::{init_syscall_stack, setup_syscall};
    init_syscall_stack();
    setup_syscall();
}
