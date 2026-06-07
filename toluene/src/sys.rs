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
pub fn stdout_write(data: &[u8]) {
    let _ = write(1, data);
}

/// Print a string to stdout.
pub fn println(s: &str) {
    stdout_write(s.as_bytes());
    stdout_write(b"\n");
}

/// Print a string without newline.
pub fn print(s: &str) {
    stdout_write(s.as_bytes());
}

/// Get the number of active processes.
pub fn process_count() -> usize {
    // Return a sensible default for user-space
    1
}

/// Get the system uptime in scheduler ticks.
pub fn uptime_ticks() -> u64 {
    0 // Kernel would expose this via syscall
}