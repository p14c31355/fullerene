//! Built-in shell commands for Nozzle

use crate::exec::CommandContext;
use alloc::format;

/// `clear` — clear the terminal screen
pub fn cmd_clear(ctx: &mut CommandContext) -> bool {
    ctx.terminal.write_str("\x1b[2J\x1b[H");
    true
}

/// `echo` — print arguments back to the terminal
pub fn cmd_echo(ctx: &mut CommandContext) -> bool {
    for (i, arg) in ctx.args.iter().enumerate() {
        if i == 0 {
            continue;
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
    false
}

/// `uname` — show system information
pub fn cmd_uname(ctx: &mut CommandContext) -> bool {
    ctx.terminal.write_str("Fullerene (Nozzle) 0.2.0 x86_64\n");
    true
}

/// `ls` — list files (stub)
pub fn cmd_ls(_ctx: &mut CommandContext) -> bool {
    _ctx.terminal.write_str("(no filesystem mounted)\n");
    true
}

/// `cat` — print file contents (stub)
pub fn cmd_cat(ctx: &mut CommandContext) -> bool {
    if ctx.args.len() < 2 {
        ctx.terminal.write_str("Usage: cat <file>\n");
        return true;
    }
    ctx.terminal.write_str("(no filesystem mounted: ");
    ctx.terminal.write_str(ctx.args[1]);
    ctx.terminal.write_str(")\n");
    true
}

/// `pwd` — print working directory
pub fn cmd_pwd(ctx: &mut CommandContext) -> bool {
    ctx.terminal.write_str("/\n");
    true
}

/// `meminfo` — display memory information (stub)
pub fn cmd_meminfo(ctx: &mut CommandContext) -> bool {
    ctx.terminal
        .write_str("Memory info not available from userland\n");
    true
}

/// `dmesg` — display kernel message buffer (stub)
pub fn cmd_dmesg(ctx: &mut CommandContext) -> bool {
    ctx.terminal
        .write_str("[dmesg] kernel ring buffer not yet implemented\n");
    true
}

/// `ps` — list processes (stub)
pub fn cmd_ps(ctx: &mut CommandContext) -> bool {
    ctx.terminal.write_str("  PID STATE    NAME\n");
    ctx.terminal.write_str("  --- -----    ----\n");
    ctx.terminal.write_str("    0 RUNNING  kernel\n");
    ctx.terminal.write_str("    1 RUNNING  shell\n");
    true
}

/// `hexdump` — show hex dump of provided string
pub fn cmd_hexdump(ctx: &mut CommandContext) -> bool {
    if ctx.args.len() < 2 {
        ctx.terminal.write_str("Usage: hexdump <text>\n");
        return true;
    }
    let input = ctx.args[1];
    for byte in input.bytes() {
        let s = format!("{:02x} ", byte);
        ctx.terminal.write_str(&s);
    }
    ctx.terminal.write_str("\n");
    true
}

/// `version` — show fullerene version
pub fn cmd_version(ctx: &mut CommandContext) -> bool {
    ctx.terminal.write_str("Fullerene 0.2.0\n");
    ctx.terminal.write_str("Built: 2026-05-26\n");
    ctx.terminal
        .write_str("Components: Lattice, Nozzle, Solvent, ChronoLine, Resonance\n");
    true
}
