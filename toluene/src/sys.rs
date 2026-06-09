//! System call wrappers for Toluene SDK.
//!
//! Thin wrappers around the petroleum syscall interface that provide
//! ergonomic Rust APIs for user-space programs.

use petroleum::common::syscall::{exit, getpid, sleep, write};

/// Get the current process ID.
pub fn current_pid() -> usize {
    getpid() as usize
}

/// Yield the CPU to the scheduler.
pub fn yield_now() {
    sleep();
}

/// Terminate the process with an exit code.
pub fn exit_process(code: i32) -> ! {
    exit(code);
}

/// Write raw bytes to stdout (fd 1).
pub fn stdout_write(data: &[u8]) -> Result<usize, i64> {
    let result = write(1, data);
    if result < 0 {
        Err(result)
    } else {
        Ok(result as usize)
    }
}

/// Print a string to stdout.
pub fn println(s: &str) {
    let _ = stdout_write(s.as_bytes());
    let _ = stdout_write(b"\n");
}

/// Print a string without newline.
pub fn print(s: &str) {
    let _ = stdout_write(s.as_bytes());
}

/// Get the number of active processes.
/// Returns None if the syscall is not available.
pub fn process_count() -> Option<usize> {
    // No syscall available yet
    None
}

/// Get the system uptime in scheduler ticks.
/// Returns None if the syscall is not available.
pub fn uptime_ticks() -> Option<u64> {
    // No syscall available yet
    None
}
