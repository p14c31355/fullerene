//! Shell/command line interface for Fullerene OS
//!
//! Thin wrapper around the [`nozzle`] shell runtime.  Provides a
//! `KernelTerminal` that bridges the abstract `nozzle::Terminal`
//! trait to the kernel’s raw syscall I/O.

use crate::syscall::kernel_syscall;

/// Initialize the shell subsystem (formerly keyboard init, etc.)
pub fn init() {
    nitrogen::ps2::keyboard::init_keyboard();
    petroleum::serial::serial_log(format_args!("Shell/CLI initialized\n"));
}

/// Main shell entry point — called from the scheduler as a kernel process.
pub fn shell_main() {
    use nozzle::Shell;

    petroleum::debug_log!("Shell main started");

    // Try to use the Lattice-backed GUI terminal via Solvent runtime first
    if solvent::is_initialized() {
        let mut term = solvent::LatticeTerminal;
        let commands = nozzle::default_commands();
        let mut shell = Shell::new(&mut term, commands);
        shell.set_prompt("fullerene> ");
        shell.run();
    } else {
        // Fallback to kernel syscall terminal
        let mut term = KernelTerminal;
        let commands = nozzle::default_commands();
        let mut shell = Shell::new(&mut term, commands);
        shell.set_prompt("fullerene> ");
        shell.run();
    }
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
        // Block until a byte is available (yielding to other processes).
        loop {
            let mut byte = 0u8;
            let res = kernel_syscall(3, 0, &mut byte as *mut u8 as u64, 1);
            if res > 0 {
                return Some(byte);
            }
            // Yield to other processes and retry
            kernel_syscall(22, 0, 0, 0);
        }
    }

    fn input_available(&self) -> bool {
        nitrogen::ps2::keyboard::input_available()
    }
}
