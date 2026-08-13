#![no_std]
#![no_main]

//! Minimal userland init/supervisor.
//!
//! PID 1 owns the policy of keeping the interactive shell alive.  It creates
//! the terminal endpoint and spawns the shell through the same native ABI
//! available to every other user process.

use core::arch::asm;

const EXIT: u64 = 1;
const WRITE: u64 = 4;
const WAIT: u64 = 7;
const YIELD: u64 = 22;
const SPAWN: u64 = 23;
const HANDLE_REVOKE: u64 = 92;
const CREATE_TERMINAL: u64 = 65;
const OPEN_PROCESS_CONTROL: u64 = 110;
const PROCESS_CONTROL_STATUS: u64 = 112;
const PROCESS_CONTROL_REAP: u64 = 113;

static SHELL_IMAGE: &[u8] = include_bytes!(env!("FULLERENE_SHELL_IMAGE"));

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_eh_personality() {}

unsafe fn syscall(number: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> u64 {
    let result: u64;
    unsafe {
        asm!(
            "syscall",
            in("rax") number,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            in("r10") a4,
            in("r8") a5,
            in("r9") a6,
            lateout("rax") result,
            out("rcx") _,
            out("r11") _,
        );
    }
    result
}

fn write(message: &[u8]) {
    unsafe {
        let _ = syscall(
            WRITE,
            1,
            message.as_ptr() as u64,
            message.len() as u64,
            0,
            0,
            0,
        );
    }
}

fn launch_shell() -> u64 {
    let title = b"Fullerene Shell";
    let terminal = unsafe {
        syscall(
            CREATE_TERMINAL,
            title.as_ptr() as u64,
            title.len() as u64,
            0,
            0,
            0,
            0,
        )
    };
    if (terminal as i64) < 0 {
        return terminal;
    }
    let name = b"shell";
    unsafe {
        syscall(
            SPAWN,
            SHELL_IMAGE.as_ptr() as u64,
            SHELL_IMAGE.len() as u64,
            name.as_ptr() as u64,
            name.len() as u64,
            terminal,
            0,
        )
    }
}

fn init() -> ! {
    write(b"launchd: PID 1 started\n");
    loop {
        let child = launch_shell();
        if (child as i64) < 0 {
            write(b"launchd: shell start failed\n");
            unsafe { syscall(EXIT, 1, 0, 0, 0, 0, 0) };
        }
        let control = unsafe { syscall(OPEN_PROCESS_CONTROL, child, 0, 0, 0, 0, 0) };
        if (control as i64) >= 0 {
            // The supervisor observes and reaps through its capability. The
            // parent relationship is still maintained by the kernel, but it
            // is not used as the administration mechanism here.
            loop {
                let status = unsafe { syscall(PROCESS_CONTROL_STATUS, control, 0, 0, 0, 0, 0) };
                if (status as i64) < 0 {
                    let _ = unsafe { syscall(WAIT, child, 0, 0, 0, 0, 0) };
                    break;
                }
                if status == 3 {
                    let reaped = unsafe { syscall(PROCESS_CONTROL_REAP, control, 0, 0, 0, 0, 0) };
                    if (reaped as i64) < 0 {
                        // A failed capability reap must not leave a zombie
                        // behind. The birth parent remains a safe fallback.
                        let _ = unsafe { syscall(WAIT, child, 0, 0, 0, 0, 0) };
                    }
                    break;
                }
                unsafe {
                    let _ = syscall(YIELD, 0, 0, 0, 0, 0, 0);
                }
            }
            let _ = unsafe { syscall(HANDLE_REVOKE, control, 0, 0, 0, 0, 0) };
        } else {
            // Never restart with an unmanaged child left behind.
            write(b"launchd: control open failed; waiting\n");
            let _ = unsafe { syscall(WAIT, child, 0, 0, 0, 0, 0) };
        }
        write(b"launchd: shell exited; restarting\n");
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    init()
}
