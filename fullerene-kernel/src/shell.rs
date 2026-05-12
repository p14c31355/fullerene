//! Basic shell/command line interface for Fullerene OS
//!
//! This module provides a simple command-line interface that allows users
//! to interact with the operating system through text commands.

use crate::keyboard;
use crate::scheduler::get_system_tick;
use crate::syscall::kernel_syscall;
use alloc::{format, string::String, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};
use petroleum::{define_commands, print};

/// Shell prompt
const PROMPT: &str = "fullerene> ";

/// Command function type
type CommandFn = fn(&[&str]) -> i32;

/// Command entry
#[derive(Debug)]
struct CommandEntry {
    name: &'static str,
    description: &'static str,
    function: CommandFn,
}

/// Direct syscall wrapper for user mode
fn user_syscall(num: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let res: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") num,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            lateout("rax") res,
        );
    }
    res
}

/// Helper for writing to stdout via syscall
fn user_print(s: &str) {
    user_syscall(4, 1, s.as_ptr() as u64, s.len() as u64);
}

static COMMANDS: &[CommandEntry] = define_commands!(
    CommandEntry,
    ("help", "Show available commands", help_command),
    ("ps", "Show process list", not_implemented_command),
    ("top", "Show top processes", not_implemented_command),
    ("free", "Show memory usage", not_implemented_command),
    ("uptime", "Show system uptime", uptime_command),
    ("date", "Show current date/time", date_command),
    ("history", "Show command history", history_command),
    ("echo", "Print text", echo_command),
    ("clear", "Clear screen", clear_command),
    ("uname", "Show system information", uname_command),
    ("kill", "Kill a process (usage: kill <pid>)", not_implemented_command),
    ("exit", "Exit shell", exit_command)
);

// Shell main loop
pub fn shell_main() {
    petroleum::debug_log!("Shell main started");
    petroleum::shell_response!("Welcome to Fullerene OS Shell\n");
    petroleum::shell_response!("Type 'help' for available commands.\n\n");

    loop {
        // Print prompt
        user_print("fullerene> ");

        // Read line from keyboard
        petroleum::debug_log!("About to read line from keyboard");
        let mut input_buffer = [0u8; 256];
        match read_line(&mut input_buffer) {
            Ok(len) => {
                petroleum::debug_log!("read_line returned len: {}", len);
                let line = &input_buffer[..len];

                // Convert to string and process
                match core::str::from_utf8(line) {
                    Ok(line_str) => {
                        petroleum::debug_log!("Processed line: '{}'", line_str);
                        if !process_command(line_str.trim()) {
                            break; // Exit shell
                        }
                    }
                    Err(_) => {
                        print!("Invalid UTF-8 input\n");
                    }
                }
            }
            Err(_) => {
                print!("Input error\n");
            }
        }
    }

    user_print("Shell exited.\n");
}

// Read a line with echo and editing
fn read_line(buffer: &mut [u8]) -> Result<usize, &'static str> {
    let mut pos = 0;
    let max_len = buffer.len();

    while pos < max_len {
        let mut byte = [0u8; 1];
        let res = user_syscall(3, 0, byte.as_mut_ptr() as u64, 1);
        if res > 0 {
            let ch = byte[0];
            match ch {
                b'\n' | b'\r' => {
                    // Enter - finish line
                    user_print("\n");
                    break;
                }
                0x08 => {
                    // Backspace
                    if pos > 0 {
                        pos -= 1;
                        // Echo backspace
                        user_print("\x08 \x08");
                    }
                }
                0x1B => {
                    // Escape sequences - skip for now
                    continue;
                }
                ch if ch.is_ascii() && !ch.is_ascii_control() => {
                    // Printable character
                    buffer[pos] = ch;
                    pos += 1;

                    // Echo character
                    user_print(&format!("{}", ch as char));
                }
                _ => {} // Ignore other characters
            }
        }

        // Improved polling: yield multiple times to reduce CPU usage
        for _ in 0..10 {
            kernel_syscall(22, 0, 0, 0); // Yield
        }
    }

    Ok(pos)
}

// Process a command line
fn process_command(line: &str) -> bool {
    if line.is_empty() {
        return true;
    }

    // Split into arguments
    let args: Vec<&str> = line.split_whitespace().collect();

    if args.is_empty() {
        return true;
    }

    let command_name = args[0];

    // Find and execute command
    for cmd in COMMANDS {
        if cmd.name == command_name {
            let exit_code = (cmd.function)(&args);
            if command_name == "exit" || exit_code != 0 {
                return false; // Exit shell
            }
            return true;
        }
    }

    print!("Unknown command: {}\n", command_name);
    print!("Type 'help' for available commands.\n");
    true
}

fn not_implemented_command(args: &[&str]) -> i32 {
    user_print(&format!("{}: Not implemented for user mode\n", args[0]));
    0
}

fn help_command(_args: &[&str]) -> i32 {
    print!("Available commands:\n");
    for cmd in COMMANDS {
        print!("  {:10} - {}\n", cmd.name, cmd.description);
    }
    0
}

fn echo_command(args: &[&str]) -> i32 {
    if args.len() < 2 {
        print!("\n");
    } else {
        for arg in &args[1..] { print!("{} ", arg); }
        print!("\n");
    }
    0
}

petroleum::simple_command_fn!(clear_command, "\x1b[2J\x1b[H");
petroleum::simple_command_fn!(uname_command, "Fullerene OS 0.1.0 x86_64\n");

fn uptime_command(_args: &[&str]) -> i32 {
    let ticks = get_system_tick();
    let s = ticks / 1000;
    print!("Uptime: {:02}:{:02}:{:02} ({} ticks)\n", s/3600, (s%3600)/60, s%60, ticks);
    0
}

fn date_command(_args: &[&str]) -> i32 {
    print!("Current date/time: System tick: {}\n(RTC integration pending)\n", get_system_tick());
    0
}

fn history_command(_args: &[&str]) -> i32 {
    print!("Command history: (Not implemented)\nUse 'help' for available commands.\n");
    0
}

fn exit_command(_args: &[&str]) -> i32 {
    print!("Exiting shell...\n");
    1
}

// Helper to print to stdout (since we can't use println! in kernel)
// fn print implementation removed in favor of petroleum::print macro

// Initialize shell module
pub fn init() {
    // Nothing to initialize for basic shell
    crate::keyboard::init();
    petroleum::serial::serial_log(format_args!("Shell/CLI initialized\n"));
}

// Test functions
#[cfg(test)]
mod tests {

    #[test]
    fn test_command_parsing() {
        // Basic tests would go here
        // For now, we trust the runtime behavior
    }
}
