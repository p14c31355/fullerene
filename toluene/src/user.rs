//! User space system call wrappers for toluene

#![no_std]

/// Simple system call wrapper (for user space programs)
#[inline]
unsafe fn syscall(syscall_num: u64, arg1: u64, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> u64 {
    let result: u64;
    // Use interrupt 0x80 for system calls (Linux/BSD style)
    core::arch::asm!(
        "int 0x80",
        in("rax") syscall_num,
        in("rbx") arg1,
        in("rcx") arg2,
        in("rdx") arg3,
        in("rsi") arg4,
        in("rdi") arg5,
        lateout("rax") result,
    );
    result
}

/// Simple write syscall wrapper
pub fn write(fd: i32, buf: &[u8]) -> i64 {
    unsafe {
        syscall(
            4, // SYS_WRITE
            fd as u64,
            buf.as_ptr() as u64,
            buf.len() as u64,
            0,
            0,
        ) as i64
    }
}

/// Simple exit syscall wrapper
pub fn exit(code: i32) {
    unsafe {
        syscall(1, code as u64, 0, 0, 0, 0);
    }
}

/// Get PID syscall wrapper
pub fn getpid() -> u64 {
    unsafe { syscall(20, 0, 0, 0, 0, 0) }
}

/// Yield syscall wrapper
pub fn sleep() {
    unsafe {
        syscall(22, 0, 0, 0, 0, 0);
    }
}
