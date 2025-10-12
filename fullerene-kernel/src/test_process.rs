//! Test process module containing the test user process functions

// Test process main function
pub fn test_process_main() {
    // Simple test process that demonstrates system calls using proper syscall instruction
    unsafe fn syscall(
        num: u64,
        arg1: u64,
        arg2: u64,
        arg3: u64,
        arg4: u64,
        arg5: u64,
        arg6: u64,
    ) -> u64 {
        let result: u64;
        unsafe { core::arch::asm!(
            "syscall",
            in("rax") num,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rdx") arg3,
            in("r10") arg4,
            in("r8") arg5,
            in("r9") arg6,
            lateout("rax") result,
            out("rcx") _, out("r11") _,
        ); }
        result
    }

    // Write to stdout via syscall
    let message = b"Hello from test user process!\n";
    unsafe {
        syscall(
            crate::syscall::SyscallNumber::Write as u64,
            1, // fd (stdout)
            message.as_ptr() as u64,
            message.len() as u64,
            0,
            0,
            0,
        );
    }

    // Get PID via syscall and print the actual PID
    unsafe {
        let pid = syscall(
            crate::syscall::SyscallNumber::GetPid as u64,
            0,
            0,
            0,
            0,
            0,
            0,
        );
        let pid_msg = b"My PID is: ";
        syscall(
            crate::syscall::SyscallNumber::Write as u64,
            1,
            pid_msg.as_ptr() as u64,
            pid_msg.len() as u64,
            0,
            0,
            0,
        );

        // Convert PID to string and print it
        let pid_str = alloc::format!("{}\n", pid);
        let pid_bytes = pid_str.as_bytes();
        syscall(
            crate::syscall::SyscallNumber::Write as u64,
            1,
            pid_bytes.as_ptr() as u64,
            pid_bytes.len() as u64,
            0,
            0,
            0,
        );
    }

    // Yield a bit
    unsafe {
        syscall(
            crate::syscall::SyscallNumber::Yield as u64,
            0,
            0,
            0,
            0,
            0,
            0,
        ); // SYS_YIELD
        syscall(
            crate::syscall::SyscallNumber::Yield as u64,
            0,
            0,
            0,
            0,
            0,
            0,
        ); // SYS_YIELD
    }

    // Exit
    unsafe {
        syscall(crate::syscall::SyscallNumber::Exit as u64, 0, 0, 0, 0, 0, 0); // SYS_EXIT
    }
}
