use fullerene_abi::SyscallNumber;

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

    let result = match SyscallNumber::try_from(syscall_num) {
        Ok(SyscallNumber::AbiQuery) => basic::syscall_abi_query(arg1 as *mut u8, arg2 as usize),

        Ok(SyscallNumber::Exit) => basic::syscall_exit(arg1 as i32),
        Ok(SyscallNumber::Fork) => basic::syscall_fork(),
        Ok(SyscallNumber::Read) => {
            basic::syscall_read(arg1 as core::ffi::c_int, arg2 as *mut u8, arg3 as usize)
        }
        Ok(SyscallNumber::Write) => {
            basic::syscall_write(arg1 as core::ffi::c_int, arg2 as *const u8, arg3 as usize)
        }
        Ok(SyscallNumber::Open) => {
            basic::syscall_open(arg1 as *const u8, arg2 as core::ffi::c_int, arg3 as u32)
        }
        Ok(SyscallNumber::Close) => basic::syscall_close(arg1 as core::ffi::c_int),
        Ok(SyscallNumber::Wait) => basic::syscall_wait(arg1),
        Ok(SyscallNumber::GetPid) => basic::syscall_getpid(),
        Ok(SyscallNumber::GetProcessName) => {
            basic::syscall_get_process_name(arg1 as *mut u8, arg2 as usize)
        }
        Ok(SyscallNumber::Yield) => basic::syscall_yield(),

        Ok(SyscallNumber::MapMemory) => memory::syscall_map_memory(arg1, arg2, arg3),
        Ok(SyscallNumber::UnmapMemory) => memory::syscall_unmap_memory(arg1, arg2),
        Ok(SyscallNumber::ProtectMemory) => memory::syscall_protect_memory(arg1, arg2, arg3),
        Ok(SyscallNumber::QueryMemory) => {
            memory::syscall_query_memory(arg1 as *mut u8, arg2 as usize)
        }

        Ok(SyscallNumber::CreateEvent) => event::syscall_create_event(arg1),
        Ok(SyscallNumber::WaitEvent) => event::syscall_wait_event(arg1, arg2),
        Ok(SyscallNumber::SignalEvent) => event::syscall_signal_event(arg1),
        Ok(SyscallNumber::SubscribeEvent) => event::syscall_subscribe_event(arg1, arg2),

        Ok(SyscallNumber::CreateThread) => thread::syscall_create_thread(arg1, arg2, arg3),
        Ok(SyscallNumber::JoinThread) => thread::syscall_join_thread(arg1),
        Ok(SyscallNumber::DetachThread) => thread::syscall_detach_thread(arg1),
        Ok(SyscallNumber::ExitThread) => thread::syscall_exit_thread(arg1 as i32),

        Ok(SyscallNumber::CreateWindow) => {
            window::syscall_create_window(arg1 as i32, arg2 as i32, arg3 as u32, arg4 as u32, arg5)
        }
        Ok(SyscallNumber::DestroyWindow) => window::syscall_destroy_window(arg1),
        Ok(SyscallNumber::ResizeWindow) => {
            window::syscall_resize_window(arg1, arg2 as u32, arg3 as u32)
        }
        Ok(SyscallNumber::PresentWindow) => window::syscall_present_window(arg1),
        Ok(SyscallNumber::GetWindowEvent) => {
            window::syscall_get_window_event(arg1, arg2 as *mut u8, arg3 as usize)
        }

        Ok(SyscallNumber::EnumerateDevices) => {
            device::syscall_enumerate_devices(arg1, arg2 as *mut u8, arg3 as usize)
        }
        Ok(SyscallNumber::OpenDevice) => device::syscall_open_device(arg1 as *const u8),
        Ok(SyscallNumber::DeviceIoctl) => device::syscall_device_ioctl(arg1, arg2, arg3),

        Ok(SyscallNumber::ChannelCreate) => ipc::syscall_channel_create(arg1),
        Ok(SyscallNumber::ChannelSend) => ipc::syscall_channel_send(arg1, arg2 as *const u8, arg3),
        Ok(SyscallNumber::ChannelRecv) => ipc::syscall_channel_recv(arg1, arg2 as *mut u8, arg3),
        Ok(SyscallNumber::PipeCreate) => ipc::syscall_pipe_create(arg1 as *mut u64),

        Ok(SyscallNumber::HandleTransfer) => cap::syscall_handle_transfer(arg1, arg2),
        Ok(SyscallNumber::HandleDuplicate) => cap::syscall_handle_duplicate(arg1),
        Ok(SyscallNumber::HandleRevoke) => cap::syscall_handle_revoke(arg1),

        Ok(SyscallNumber::ClockGetTime) => time::syscall_clock_gettime(arg1, arg2 as *mut u8),
        Ok(SyscallNumber::TimerCreate) => time::syscall_timer_create(arg1, arg2, arg3),
        Ok(SyscallNumber::Sleep) => time::syscall_sleep(arg1),
        Ok(SyscallNumber::Uptime) => time::syscall_uptime(arg1 as *mut u8),

        Err(()) => Err(SyscallError::InvalidSyscall),
    };

    match result {
        Ok(value) => value,
        Err(error) => (-(error as i64)) as u64,
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
