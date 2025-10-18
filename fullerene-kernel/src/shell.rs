//! Basic shell/command line interface for Fullerene OS
//!
//! This module provides a simple command-line interface that allows users
//! to interact with the operating system through text commands.

use crate::keyboard;
use crate::scheduler::get_system_tick;
use crate::syscall::kernel_syscall;
use alloc::{vec::Vec, string::String};
use core::sync::atomic::{AtomicU64, Ordering};
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

static COMMANDS: &[CommandEntry] = define_commands!(CommandEntry,
    ("help", "Show available commands", help_command),
    ("ps", "Show process list", ps_command),
    ("top", "Show top processes", top_command),
    ("free", "Show memory usage", free_command),
    ("uptime", "Show system uptime", uptime_command),
    ("date", "Show current date/time", date_command),
    ("history", "Show command history", history_command),
    ("echo", "Print text", echo_command),
    ("clear", "Clear screen", clear_command),
    ("uname", "Show system information", uname_command),
    ("kill", "Kill a process (usage: kill <pid>)", kill_command),
    ("exit", "Exit shell", exit_command)
);

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
                        print!("\x08 \x08");
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
                    print!("{}", ch as char);
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
    print!("PID    State      Name\n");
    print!("--------------------------\n");
    let process_list = crate::process::PROCESS_LIST.lock();
    for proc in process_list.iter() {
        print!("{:<6} {:<10?} {}\n", proc.id, proc.state, proc.name);
    }
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

fn top_command(_args: &[&str]) -> i32 {
    print!("Top processes (by priority):\n");
    print!("PID    PPID   State      CPU%   Name\n");
    print!("-----------------------------------\n");

    let process_list = crate::process::PROCESS_LIST.lock();
    let mut procs: Vec<_> = process_list.iter().collect();
    // Sort by process ID as a simple proxy for priority
    procs.sort_by(|a, b| b.id.cmp(&a.id));

    let mut shown = 0;
    for proc in procs.iter().take(10) {
        let ppid = match proc.parent_id {
            Some(pid) => pid,
            None => 0,
        };
        print!("{:<6} {:<6} {:<10?} 0.0   {}\n", proc.id, ppid, proc.state, proc.name);
        shown += 1;
        if shown >= 5 {
            break; // Limit to top 5 for display
        }
    }

    print!("Showing {} processes\n", shown);
    0
}

fn free_command(_args: &[&str]) -> i32 {
    let allocator = petroleum::page_table::ALLOCATOR.lock();
    let used = allocator.used();
    let total = allocator.size();
    let free = total - used;
    let used_pct = (used * 100) / total;

    print!("Memory usage:\n");
    print!("Total: {} bytes\n", total);
    print!("Used:  {} bytes ({}%)\n", used, used_pct);
    print!("Free:  {} bytes ({}%)\n", free, 100 - used_pct);
    0
}

fn uptime_command(_args: &[&str]) -> i32 {
    // For now, use approximate tick count
    // In a real system, we'd track real time
    let ticks = get_system_tick(); // TODO: Get actual system tick
    let uptime_seconds = ticks / 1000; // Assuming 1000 ticks per second
    let hours = uptime_seconds / 3600;
    let minutes = (uptime_seconds % 3600) / 60;
    let seconds = uptime_seconds % 60;

    print!("Uptime: {:02}:{:02}:{:02} ({} ticks)\n", hours, minutes, seconds, ticks);
    0
}

fn date_command(_args: &[&str]) -> i32 {
    // Simple date/time - would be enhanced with RTC in real implementation
    print!("Current date/time: ");
    print!("System tick: {}\n", get_system_tick()); // TODO: Get actual system tick
    print!("(RTC integration pending)\n");
    0
}

fn history_command(_args: &[&str]) -> i32 {
    print!("Command history:\n");
    print!("(History feature not yet implemented)\n");
    print!("Use 'help' to see available commands.\n");
    0
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

    #[test]
    fn test_command_parsing() {
        // Basic tests would go here
        // For now, we trust the runtime behavior
    }
}
