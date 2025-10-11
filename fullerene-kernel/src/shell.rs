//! Basic shell/command line interface for Fullerene OS
//!
//! This module provides a simple command-line interface that allows users
//! to interact with the operating system through text commands.

#![no_std]

use crate::keyboard;
use crate::syscall::{self, kernel_syscall};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use petroleum::print;

/// Shell prompt
const PROMPT: &str = "fullerene> ";

/// Command function type
type CommandFn = fn(&[&str]) -> i32;

/// Command entry
struct CommandEntry {
    name: &'static str,
    description: &'static str,
    function: CommandFn,
}

static COMMANDS: &[CommandEntry] = &[
    CommandEntry {
        name: "help",
        description: "Show available commands",
        function: help_command,
    },
    CommandEntry {
        name: "ps",
        description: "Show process list",
        function: ps_command,
    },
    CommandEntry {
        name: "echo",
        description: "Print text",
        function: echo_command,
    },
    CommandEntry {
        name: "clear",
        description: "Clear screen",
        function: clear_command,
    },
    CommandEntry {
        name: "uname",
        description: "Show system information",
        function: uname_command,
    },
    CommandEntry {
        name: "kill",
        description: "Kill a process (usage: kill <pid>)",
        function: kill_command,
    },
    CommandEntry {
        name: "exit",
        description: "Exit shell",
        function: exit_command,
    },
];

// Shell main loop
pub fn shell_main() {
    print!("Welcome to Fullerene OS Shell");
    print!("\n");
    print!("Type 'help' for available commands.");
    print!("\n\n");

    loop {
        // Print prompt
        print!("fullerene> ");

        // Read line from keyboard
        let mut input_buffer = [0u8; 256];
        match read_line(&mut input_buffer) {
            Ok(len) => {
                let line = &input_buffer[..len];

                // Convert to string and process
                match core::str::from_utf8(line) {
                    Ok(line_str) => {
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

    print!("Shell exited.\n");
}

// Read a line with echo and editing
fn read_line(buffer: &mut [u8]) -> Result<usize, &'static str> {
    let mut pos = 0;
    let max_len = buffer.len();

    while pos < max_len {
        if let Some(ch) = keyboard::read_char() {
            match ch {
                b'\n' | b'\r' => {
                    // Enter - finish line
                    print!("\n");
                    break;
                }
                0x08 => {
                    // Backspace
                    if pos > 0 {
                        pos -= 1;
                        // Echo backspace - keeping the syscall for kernel output
                        let backspace_seq = [0x08, b' ', 0x08];
                        kernel_syscall(4, 1, backspace_seq.as_ptr() as u64, 3);
                    }
                }
                0x1B => {
                    // Escape sequences - skip for now
                    // Would handle arrow keys, etc. in full implementation
                    continue;
                }
                ch if ch.is_ascii() && !ch.is_ascii_control() => {
                    // Printable character
                    buffer[pos] = ch;
                    pos += 1;

                    // Echo character
                    kernel_syscall(4, 1, (&ch as *const _ as u64), 1);
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

// Command implementations
fn help_command(_args: &[&str]) -> i32 {
    print!("Available commands:\n");
    for cmd in COMMANDS {
        print!("  {:10} - {}\n", cmd.name, cmd.description);
    }
    0
}

fn ps_command(_args: &[&str]) -> i32 {
    print!("Process list:\n");
    print!("PID    Name\n");
    // In a full implementation, we'd list all processes
    // For now, just show idle process
    print!("001    idle\n");
    print!("002    shell\n");
    0
}

fn echo_command(args: &[&str]) -> i32 {
    if args.len() < 2 {
        print!("\n");
        return 0;
    }

    for arg in &args[1..] {
        print!("{} ", arg);
    }
    print!("\n");
    0
}

fn clear_command(_args: &[&str]) -> i32 {
    print!("\x1b[2J\x1b[H"); // ANSI clear screen and home
    0
}

fn uname_command(_args: &[&str]) -> i32 {
    print!("Fullerene OS 0.1.0 x86_64\n");
    0
}

fn kill_command(args: &[&str]) -> i32 {
    if args.len() < 2 {
        print!("Usage: kill <pid>\n");
        return 1;
    }

    match args[1].parse::<u64>() {
        Ok(pid) => {
            // Terminate process
            crate::process::terminate_process(pid, 9); // SIGKILL equivalent
            print!("Sent kill signal to process {}\n", pid);
            0
        }
        Err(_) => {
            print!("Invalid PID: {}\n", args[1]);
            1
        }
    }
}

fn exit_command(_args: &[&str]) -> i32 {
    print!("Exiting shell...\n");
    1 // Non-zero exit to signal shell termination
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
    use super::*;

    #[test]
    fn test_command_parsing() {
        // Basic tests would go here
        // For now, we trust the runtime behavior
    }
}
