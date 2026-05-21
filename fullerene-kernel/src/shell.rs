//! Shell/command line interface for Fullerene OS
//!
//! Thin wrapper around the [`nozzle`] shell runtime.  Provides a
//! `KernelTerminal` that bridges the abstract `nozzle::Terminal`
//! trait to the kernel’s raw syscall I/O.

use crate::scheduler::get_system_tick;
use crate::syscall::kernel_syscall;
use alloc::format;

/// Initialize the shell subsystem (formerly keyboard init, etc.)
pub fn init() {
    crate::keyboard::init();
    petroleum::serial::serial_log(format_args!("Shell/CLI initialized\n"));
}

/// Main shell entry point — called from the scheduler as a kernel process.
pub fn shell_main() {
    use nozzle::Shell;

    petroleum::debug_log!("Shell main started");

    // Create a kernel-backed terminal
    let mut term = KernelTerminal;

    // Build the shell instance.  We start with only the default builtins.
    // Kernel-specific commands (uptime, ps, etc.) can be added later via
    // `nozzle::define_commands!` and passed to `Shell::new`.
    let commands = nozzle::default_commands();
    let mut shell = Shell::new(&mut term, commands);
    shell.set_prompt("fullerene> ");
    shell.run();
}

// ── Kernel terminal ─────────────────────────────────────────────────

/// A [`nozzle::Terminal`] that reads/writes through the kernel’s
/// raw syscall interface (usable from kernel-space processes).
struct KernelTerminal;

impl nozzle::Terminal for KernelTerminal {
    fn write_str(&mut self, s: &str) {
        // Syscall 4 = write, fd 1 = stdout
        kernel_syscall(4, 1, s.as_ptr() as u64, s.len() as u64);
    }

    fn read_byte(&mut self) -> Option<u8> {
        // Syscall 3 = read, fd 0 = stdin
        let mut byte = 0u8;
        let res = kernel_syscall(3, 0, &mut byte as *mut u8 as u64, 1);
        if res > 0 {
            Some(byte)
        } else {
            // Yield and retry next time
            for _ in 0..10 {
                kernel_syscall(22, 0, 0, 0); // Yield
            }
            None
        }
    }

    fn input_available(&self) -> bool {
        crate::keyboard::input_available()
    }
}