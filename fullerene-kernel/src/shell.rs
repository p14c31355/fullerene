//! Shell/command line interface for Fullerene OS
//!
//! Thin wrapper around the [`nozzle`] shell runtime.  Provides a
//! `KernelTerminal` that bridges the abstract `nozzle::Terminal`
//! trait to the kernel's raw syscall I/O.

use alloc::{format, string::String};
use alloc::string::ToString;
use crate::syscall::kernel_syscall;

/// Initialize the shell subsystem (formerly keyboard init, etc.)
pub fn init() {
    nitrogen::ps2::keyboard::init_keyboard();
    register_nozzle_hooks();
    petroleum::serial::serial_log(format_args!("Shell/CLI initialized\n"));
}

// ── Nozzle hook registration ──────────────────────────────────────

/// Register kernel implementations for nozzle's filesystem and system hooks.
/// Without this, `ls`, `cat`, `pwd`, `mem`, `tasks`, etc. all fall back
/// to "(not available)" stubs.
fn register_nozzle_hooks() {
    // FS hooks — wire into kernel VFS
    nozzle::fs_hooks::set_fs_list_fn(|ctx| {
        match crate::vfs::readdir("/") {
            Ok(entries) => {
                for ent in entries {
                    let line = if ent.is_dir {
                        format!("  {}/\n", ent.name)
                    } else {
                        format!("  {}  ({} bytes)\n", ent.name, ent.size)
                    };
                    ctx.terminal.write_str(&line);
                }
            }
            Err(e) => {
                let msg = format!("ls: {}\n", e);
                ctx.terminal.write_str(&msg);
            }
        }
    });

    nozzle::fs_hooks::set_fs_read_fn(|ctx, path| {
        match crate::vfs::open(path, 0) {
            Ok(fd) => {
                let mut buf = [0u8; 512];
                loop {
                    match crate::vfs::read(fd.fd, &mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            ctx.terminal
                                .write_str(core::str::from_utf8(&buf[..n]).unwrap_or("(binary)"));
                        }
                        Err(e) => {
                            let msg = format!("cat: {}\n", e);
                            ctx.terminal.write_str(&msg);
                            break;
                        }
                    }
                }
                let _ = crate::vfs::close(fd.fd);
                ctx.terminal.write_str("\n");
            }
            Err(e) => {
                let msg = format!("cat: {}: {}\n", path, e);
                ctx.terminal.write_str(&msg);
            }
        }
    });

    nozzle::fs_hooks::set_fs_pwd_fn(|ctx| {
        ctx.terminal.write_str("/\n");
    });

    // Sys info hooks — provide real kernel data
    nozzle::sys_hooks::set_sys_info_fn(|ctx, cmd| match cmd {
        "mem" => {
            let (heap_start, heap_end) = petroleum::common::memory::get_heap_range();
            let total = if heap_end > heap_start {
                (heap_end - heap_start) / 1024
            } else {
                0
            };
            // We don't have a precise "used" counter without tracking every
            // allocation.  Estimate from heap range.
            let msg = format!(
                "Memory: heap {} KiB total (start=0x{:x}, end=0x{:x})\n",
                total, heap_start, heap_end
            );
            ctx.terminal.write_str(&msg);
        }
        "tasks" => {
            let total = crate::process::get_process_count();
            let active = crate::process::get_active_process_count();
            let msg = format!(
                "Processes: {} active / {} total\n",
                active, total
            );
            ctx.terminal.write_str(&msg);
        }
        "windows" => {
            if solvent::is_initialized() {
                ctx.terminal
                    .write_str("Windows: managed by Lattice compositor\n");
                ctx.terminal
                    .write_str("Use the GUI to interact with windows.\n");
            } else {
                ctx.terminal
                    .write_str("Windowing system not active.\n");
            }
        }
        "dmesg" => {
            ctx.terminal
                .write_str("=== Kernel trace buffer ===\n");
            let events = crate::tracing::snapshot();
            if events.is_empty() {
                ctx.terminal
                    .write_str("(no trace events recorded)\n");
            } else {
                for ev in events {
                    let cat = core::str::from_utf8(&ev.category)
                        .unwrap_or("?")
                        .trim_end_matches('\0');
                    let msg = core::str::from_utf8(&ev.message)
                        .unwrap_or("?")
                        .trim_end_matches('\0');
                    let line = format!("[{}] {}: {}\n", ev.tick, cat, msg);
                    ctx.terminal.write_str(&line);
                }
            }
        }
        _ => {
            let msg = format!("Unknown sys info command: {}\n", cmd);
            ctx.terminal.write_str(&msg);
        }
    });

    // Sys control hooks — reboot/shutdown
    nozzle::sys_hooks::set_sys_ctl_fn(|cmd| match cmd {
        "reboot" => {
            petroleum::serial::serial_log(format_args!("Reboot requested via shell\n"));
            // Keyboard controller CPU reset via port 0x64
            unsafe {
                let port: u16 = 0x64;
                // Wait for input buffer empty
                while x86_64::instructions::port::PortReadOnly::<u8>::new(port).read() & 0x02 != 0 {}
                x86_64::instructions::port::PortWriteOnly::<u8>::new(port).write(0xFEu8);
            }
        }
        "shutdown" => {
            petroleum::serial::serial_log(format_args!("Shutdown requested via shell\n"));
            // Try VM debug-exit ports (QEMU, Bochs, VirtualBox).
            // QEMU: isa-debug-exit → port 0x604 with value (code << 1) | 1
            unsafe {
                x86_64::instructions::port::PortWriteOnly::<u16>::new(0x604).write(0x2000u16);
            }
            // Bochs: port 0xB004 with "Shutdown" string
            unsafe {
                let shutdown_str = b"Shutdown";
                let mut port = x86_64::instructions::port::PortWriteOnly::<u8>::new(0xB004);
                for &byte in shutdown_str {
                    port.write(byte);
                }
            }
            // VirtualBox: port 0x4004 with value 0x3400
            unsafe {
                x86_64::instructions::port::PortWriteOnly::<u16>::new(0x4004).write(0x3400u16);
            }
            // Real hardware: ACPI PM1a_CNT is not yet implemented.
            // Requires parsing RSDP→RSDT/XSDT→FADT to locate
            // PM1a_CNT_BLOCK / SLP_TYPa / SLP_EN and writing
            //   SLP_TYPa | SLP_EN  (typically 0x2000 | 0x2000 = 0x4000)
            // to the PM1a_CNT I/O port.
            // Fallback: halt CPU (system stays powered but idle).
            loop {
                x86_64::instructions::hlt();
            }
        }
        _ => {}
    });
}

/// Main shell entry point — called from the scheduler as a kernel process.
pub fn shell_main() {
    use nozzle::Shell;

    petroleum::debug_log!("Shell main started");

    // Ensure hooks are registered before the shell runs.
    // (shell::init() may not have been called at this point,
    //  so register hooks here as a safety net.)
    register_nozzle_hooks();

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

/// A [`nozzle::Terminal`] that reads/writes through the kernel's
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