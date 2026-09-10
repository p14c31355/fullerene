#![no_std]
#![no_main]

//! Small native child used by the AArch64 launchd process-creation smoke.
//! It is deliberately a separate ELF so SPAWN exercises image copying,
//! address-space construction, EL0 entry, WRITE, and EXIT rather than merely
//! calling another function inside PID 1.

use core::arch::asm;

const EXIT: u64 = 1;
const FORK: u64 = 2;
const WRITE: u64 = 4;
const GET_PARENT_PID: u64 = 26;
const YIELD: u64 = 22;

static MESSAGE: &[u8] = b"child: spawned and running\n";
static ORPHAN_MESSAGE: &[u8] = b"child: orphan adopted by init\n";

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let fork_pid = unsafe { syscall(FORK, 0, 0, 0, 0, 0, 0) };
    if fork_pid == 0 {
        // Do not report adoption until the original parent has exited and
        // the scheduler has reparented this child to PID 1.
        while unsafe { syscall(GET_PARENT_PID, 0, 0, 0, 0, 0, 0) } != 1 {
            let _ = unsafe { syscall(YIELD, 0, 0, 0, 0, 0, 0) };
        }
        let _ = unsafe {
            syscall(
                WRITE,
                1,
                ORPHAN_MESSAGE.as_ptr() as u64,
                ORPHAN_MESSAGE.len() as u64,
                0,
                0,
                0,
            )
        };
    } else {
        let _ = unsafe {
            syscall(
                WRITE,
                1,
                MESSAGE.as_ptr() as u64,
                MESSAGE.len() as u64,
                0,
                0,
                0,
            )
        };
    }
    let _ = unsafe { syscall(EXIT, 0, 0, 0, 0, 0, 0) };
    loop {
        unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

unsafe fn syscall(
    number: u64,
    arg0: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
) -> u64 {
    let mut result = arg0;
    unsafe {
        asm!(
            "svc #0",
            inout("x0") result,
            in("x1") arg1,
            in("x2") arg2,
            in("x3") arg3,
            in("x4") arg4,
            in("x5") arg5,
            in("x8") number,
            options(nostack)
        );
    }
    result
}
