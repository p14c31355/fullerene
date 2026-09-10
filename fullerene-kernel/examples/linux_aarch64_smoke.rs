#![no_std]
#![no_main]

//! Static Linux AArch64 ABI smoke payload.
//!
//! This is not Android init and does not claim a complete Linux userspace. It
//! exists to prove that an explicitly Linux-personality task reaches the
//! Linux syscall table through the ordinary EL0/SVC path.

use core::arch::asm;

const SYS_WRITE: u64 = 64;
const SYS_READ: u64 = 63;
const SYS_OPENAT: u64 = 56;
const SYS_CLOSE: u64 = 57;
const SYS_LSEEK: u64 = 62;
const SYS_DUP: u64 = 23;
const SYS_EXIT_GROUP: u64 = 94;
const SYS_CLOCK_GETTIME: u64 = 113;
const SYS_UNAME: u64 = 160;
const SYS_PRCTL: u64 = 167;
const SYS_GETPID: u64 = 172;
const SYS_BRK: u64 = 214;
const SYS_MUNMAP: u64 = 215;
const SYS_MMAP: u64 = 222;

const PR_SET_NAME: u64 = 15;
const MAP_PRIVATE: u64 = 0x02;
const MAP_ANONYMOUS: u64 = 0x20;
const PROT_READ: u64 = 0x1;
const PROT_WRITE: u64 = 0x2;
const AT_FDCWD: u64 = u64::MAX - 99;

static START_MESSAGE: &[u8] = b"linux-smoke: Linux AArch64 personality active\n";
static MAP_MESSAGE: &[u8] = b"linux-smoke: mmap/brk/clock/uname ok\n";
static NAME: &[u8] = b"linux-smoke\0";
static MOTD_PATH: &[u8] = b"/etc/motd\0";

#[repr(C)]
struct Timespec {
    seconds: i64,
    nanoseconds: i64,
}

#[repr(C, align(16))]
struct UtsName([u8; 390]);

static mut TIMESPEC: Timespec = Timespec {
    seconds: 0,
    nanoseconds: 0,
};
static mut UTS_NAME: UtsName = UtsName([0; 390]);

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    write(START_MESSAGE);

    let fd = syscall(SYS_OPENAT, AT_FDCWD, MOTD_PATH.as_ptr() as u64, 0, 0, 0, 0);
    if (fd as i64) >= 0 {
        let mut file_bytes = [0u8; 64];
        let read = syscall(
            SYS_READ,
            fd,
            file_bytes.as_mut_ptr() as u64,
            file_bytes.len() as u64,
            0,
            0,
            0,
        );
        if (read as i64) > 0 {
            write_bytes(file_bytes.as_ptr() as u64, read);
        }
        let _ = syscall(SYS_LSEEK, fd, 0, 0, 0, 0, 0);
        let duplicate = syscall(SYS_DUP, fd, 0, 0, 0, 0, 0);
        if (duplicate as i64) >= 0 {
            let _ = syscall(SYS_CLOSE, duplicate, 0, 0, 0, 0, 0);
        }
        let _ = syscall(SYS_CLOSE, fd, 0, 0, 0, 0, 0);
    }

    let _pid = syscall(SYS_GETPID, 0, 0, 0, 0, 0, 0);
    let _ = syscall(SYS_PRCTL, PR_SET_NAME, NAME.as_ptr() as u64, 0, 0, 0, 0);
    let timespec = unsafe { core::ptr::addr_of_mut!(TIMESPEC) as u64 };
    let _ = syscall(SYS_CLOCK_GETTIME, 1, timespec, 0, 0, 0, 0);
    let uts_name = unsafe { core::ptr::addr_of_mut!(UTS_NAME) as u64 };
    let _ = syscall(SYS_UNAME, uts_name, 0, 0, 0, 0, 0);
    let _ = syscall(SYS_BRK, 0, 0, 0, 0, 0, 0);

    let mapped = syscall(
        SYS_MMAP,
        0,
        4096,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        u64::MAX,
        0,
    );
    if (mapped as i64) >= 0 {
        unsafe {
            core::ptr::copy_nonoverlapping(
                MAP_MESSAGE.as_ptr(),
                mapped as *mut u8,
                MAP_MESSAGE.len(),
            );
        }
        write_bytes(mapped, MAP_MESSAGE.len() as u64);
        let _ = syscall(SYS_MUNMAP, mapped, 4096, 0, 0, 0, 0);
    }

    let _ = syscall(SYS_EXIT_GROUP, 0, 0, 0, 0, 0, 0);
    loop {
        unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

fn write(message: &[u8]) {
    write_bytes(message.as_ptr() as u64, message.len() as u64);
}

fn write_bytes(address: u64, length: u64) {
    let _ = syscall(SYS_WRITE, 1, address, length, 0, 0, 0);
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
