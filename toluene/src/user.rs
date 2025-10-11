//! User space system call wrappers for toluene

#![no_std]

/// Simple system call wrapper (for user space programs)
#[inline]
unsafe fn syscall(
    syscall_num: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
    arg6: u64,
) -> u64 {
    let result: u64;
    // Use syscall instruction with System V ABI (x86-64)
    // RAX = syscall number, RDI/RSI/RDX/R10/R8/R9 = arguments
    core::arch::asm!(
        "syscall",
        in("rax") syscall_num,
        in("rdi") arg1,
        in("rsi") arg2,
        in("rdx") arg3,
        in("r10") arg4,
        in("r8") arg5,
        in("r9") arg6,
        lateout("rax") result,
        // syscall may clobber rcx and r11 per ABI
        out("rcx") _, out("r11") _,
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
