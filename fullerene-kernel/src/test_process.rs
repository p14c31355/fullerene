//! Test process module containing the test user process functions

// Test process main function
pub fn test_process_main() {
    // Simple test process that demonstrates system calls using proper syscall instruction
    // Write to stdout via syscall
    let message = b"Hello from test user process!\n";
    unsafe {
        petroleum::syscall(
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
        let pid = petroleum::syscall(
            crate::syscall::SyscallNumber::GetPid as u64,
            0,
            0,
            0,
            0,
            0,
            0,
        );
        let pid_msg = b"My PID is: ";
        petroleum::syscall(
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
        petroleum::syscall(
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
        petroleum::syscall(
            crate::syscall::SyscallNumber::Yield as u64,
            0,
            0,
            0,
            0,
            0,
            0,
        ); // SYS_YIELD
        petroleum::syscall(
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
        petroleum::syscall(crate::syscall::SyscallNumber::Exit as u64, 0, 0, 0, 0, 0, 0); // SYS_EXIT
    }
}
