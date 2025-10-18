/// System call numbers
#[repr(u64)]
#[derive(Debug, Clone, Copy)]
pub enum SyscallNumber {
    /// Exit the current process (exit_code in RDI)
    Exit = 1,
    /// Write to file descriptor (fd in RDI, buffer in RSI, count in RDX)
    Write = 4,
    /// Open file (filename in RDI, flags in RSI, mode in RDX)
    Open = 5,
    /// Close file descriptor (fd in RDI)
    Close = 6,
    /// Read from file descriptor (fd in RDI, buffer in RSI, count in RDX)
    Read = 3,
    /// Create a new process (entry_point in RDI)
    Fork = 2,
    /// Wait for process to finish (pid in RDI)
    Wait = 7,
    /// Get current process ID
    GetPid = 20,
    /// Get process name (buffer in RDI, size in RSI)
    GetProcessName = 21,
    /// Yield to scheduler
    Yield = 22,
}

/// Raw syscall wrapper for x86-64 syscalls. This is a common helper function.
#[inline]
pub unsafe fn syscall(
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
    unsafe {
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
    }
    result
}

/// Simple write syscall wrapper
pub fn write(fd: i32, buf: &[u8]) -> i64 {
    unsafe {
        syscall(
            SyscallNumber::Write as u64,
            fd as u64,
            buf.as_ptr() as u64,
            buf.len() as u64,
            0,
            0,
            0,
        ) as i64
    }
}

/// Simple exit syscall wrapper
pub fn exit(code: i32) -> ! {
    unsafe {
        syscall(
            SyscallNumber::Exit as u64,
            code as u64,
            0,
            0,
            0,
            0,
            0,
        );
    }
    loop {} // unreachable, but to make ! return type
}

/// Get PID syscall wrapper
pub fn getpid() -> u64 {
    unsafe { syscall(SyscallNumber::GetPid as u64, 0, 0, 0, 0, 0, 0) }
}

/// Yield syscall wrapper
pub fn sleep() {
    unsafe {
        syscall(SyscallNumber::Yield as u64, 0, 0, 0, 0, 0, 0);
    }
}

pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add() {
        assert_eq!(add(2, 3), 5);
    }
}
