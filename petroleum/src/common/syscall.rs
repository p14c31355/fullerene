/// System call numbers
#[repr(u64)]
#[derive(Debug, Clone, Copy)]
pub enum SyscallNumber {
    /// Exit the current process (exit_code in EBX)
    Exit = 1,
    /// Write to file descriptor (fd in EBX, buffer in ECX, count in EDX)
    Write = 4,
    /// Open file (filename in EBX, flags in ECX, mode in EDX)
    Open = 5,
    /// Close file descriptor (fd in EBX)
    Close = 6,
    /// Read from file descriptor (fd in EBX, buffer in ECX, count in EDX)
    Read = 3,
    /// Create a new process (entry_point in EBX)
    Fork = 2,
    /// Wait for process to finish (pid in EBX)
    Wait = 7,
    /// Get current process ID
    GetPid = 20,
    /// Get process name (buffer in EBX, size in ECX)
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
