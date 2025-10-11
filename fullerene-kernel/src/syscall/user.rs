use super::interface::SyscallNumber;

/// Syscall helper macros for user space (would be in user-space library)
#[cfg(feature = "user_space")]
pub mod user {
    use super::SyscallNumber;

    /// Make a system call (user space wrapper)
    #[inline(always)]
    pub unsafe fn syscall(
        syscall_num: SyscallNumber,
        arg1: u64,
        arg2: u64,
        arg3: u64,
        arg4: u64,
        arg5: u64,
        arg6: u64,
    ) -> u64 {
        let mut result: u64;
        core::arch::asm!(
            "syscall",
            in("rax") syscall_num as u64,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rdx") arg3,
            in("r10") arg4,
            in("r8") arg5,
            in("r9") arg6,
            lateout("rax") result,
            // syscall clobbers rcx and r11
            out("rcx") _,
            out("r11") _,
        );
        result
    }

    /// Exit wrapper
    pub fn exit(code: i32) -> ! {
        unsafe { syscall(SyscallNumber::Exit, code as u64, 0, 0, 0, 0, 0) };
        loop {} // Should not reach here
    }

    /// Write wrapper
    pub fn write(fd: i32, buf: &[u8]) -> i64 {
        unsafe {
            syscall(
                SyscallNumber::Write,
                fd as u64,
                buf.as_ptr() as u64,
                buf.len() as u64,
                0,
                0,
                0,
            ) as i64
        }
    }

    /// Get PID wrapper
    pub fn getpid() -> u64 {
        unsafe { syscall(SyscallNumber::GetPid, 0, 0, 0, 0, 0, 0) }
    }
}
