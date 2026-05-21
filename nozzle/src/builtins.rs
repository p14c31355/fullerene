//! Built-in shell commands for Nozzle
//!
//! These commands are registered with the shell by default and provide
//! the core interactive experience.

use crate::exec::CommandContext;

/// `clear` — clear the terminal screen
pub fn cmd_clear(ctx: &mut CommandContext) -> bool {
    // VT100 escape: clear screen and move cursor home
    ctx.terminal.write_str("\x1b[2J\x1b[H");
    true
}

/// `echo` — print arguments back to the terminal
pub fn cmd_echo(ctx: &mut CommandContext) -> bool {
    for (i, arg) in ctx.args.iter().enumerate() {
        if i == 0 {
            continue; // skip command name
        }
        ctx.terminal.write_str(arg);
        if i < ctx.args.len() - 1 {
            ctx.terminal.write_str(" ");
        }
    }
    ctx.terminal.write_str("\n");
    true
}

/// `exit` — exit the shell
pub fn cmd_exit(ctx: &mut CommandContext) -> bool {
    ctx.terminal.write_str("Exiting shell...\n");
    false // signal the main loop to stop
}

/// `uname` — show system information
pub fn cmd_uname(ctx: &mut CommandContext) -> bool {
    ctx.terminal.write_str("Nozzle 0.1.0 x86_64\n");
    true
}