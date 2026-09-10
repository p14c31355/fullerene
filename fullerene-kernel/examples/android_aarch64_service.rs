#![no_std]
#![no_main]

//! Small static service used by the Rust Android-init service-manager smoke.
//! It is intentionally boring: successful `execve` and resident execution are
//! the contract under test, not a second init implementation.

use core::arch::asm;

const SYS_WRITE: u64 = 64;
const SYS_READ: u64 = 63;
const SYS_CLOSE: u64 = 57;
const SYS_OPENAT: u64 = 56;
const SYS_PRCTL: u64 = 167;
const PR_SET_NAME: u64 = 15;
const AT_FDCWD: u64 = (-100i64) as u64;

static START: &[u8] = b"fullerene-service: fullerened active\n";
static SELINUX_OK: &[u8] = b"fullerene-service: seclabel fullerened ok\n";
static SELINUX_FAILED: &[u8] = b"fullerene-service: seclabel fullerened failed\n";
static SELINUX_CURRENT: &[u8] = b"/proc/self/attr/current\0";
static NAME: &[u8] = b"fullerened\0";

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let _ = syscall(SYS_PRCTL, PR_SET_NAME, NAME.as_ptr() as u64, 0, 0, 0, 0);
    let _ = syscall(
        SYS_WRITE,
        1,
        START.as_ptr() as u64,
        START.len() as u64,
        0,
        0,
        0,
    );
    if current_context_is_fullerened() {
        let _ = syscall(
            SYS_WRITE,
            1,
            SELINUX_OK.as_ptr() as u64,
            SELINUX_OK.len() as u64,
            0,
            0,
            0,
        );
    } else {
        let _ = syscall(
            SYS_WRITE,
            1,
            SELINUX_FAILED.as_ptr() as u64,
            SELINUX_FAILED.len() as u64,
            0,
            0,
            0,
        );
    }
    loop {
        unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

fn current_context_is_fullerened() -> bool {
    let fd = syscall(
        SYS_OPENAT,
        AT_FDCWD,
        SELINUX_CURRENT.as_ptr() as u64,
        0,
        0,
        0,
        0,
    );
    if (fd as i64) < 0 {
        return false;
    }
    let mut context = [0u8; 64];
    let read = syscall(
        SYS_READ,
        fd,
        context.as_mut_ptr() as u64,
        context.len() as u64,
        0,
        0,
        0,
    );
    let closed = syscall(SYS_CLOSE, fd, 0, 0, 0, 0, 0) as i64 == 0;
    read == b"u:r:fullerened:s0\0".len() as u64
        && context[..read as usize] == *b"u:r:fullerened:s0\0"
        && closed
}

fn syscall(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> u64 {
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
