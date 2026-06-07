//! Built-in shell commands for Nozzle
//!
//! Each command receives a `CommandContext` with the terminal and arguments.
//! Return `true` to continue the shell, `false` to exit.

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
    ctx.terminal.write_str("Fullerene (Nozzle) 0.3.0 x86_64\n");
    true
}

/// `ls` — list files in current directory
///
/// This command dispatches to the kernel-provided filesystem list function
/// set via `set_fs_list_fn`.  When no filesystem is mounted, a stub message
/// is shown.
pub fn cmd_ls(ctx: &mut CommandContext) -> bool {
    crate::fs_hooks::list_directory(ctx);
    true
}

/// `cat` — print file contents
pub fn cmd_cat(ctx: &mut CommandContext) -> bool {
    if ctx.args.len() < 2 {
        ctx.terminal.write_str("Usage: cat <file>\n");
        return true;
    }
    crate::fs_hooks::read_file(ctx, ctx.args[1]);
    true
}

/// `pwd` — print working directory
pub fn cmd_pwd(ctx: &mut CommandContext) -> bool {
    crate::fs_hooks::print_working_directory(ctx);
    true
}

/// `mem` — display memory information (replaces `meminfo` stub)
///
/// Dispatches to the kernel-provided `SYS_INFO_FN` hook.
pub fn cmd_mem(ctx: &mut CommandContext) -> bool {
    crate::sys_hooks::call_sys_info_hook(ctx, "mem");
    true
}

/// `tasks` — list processes (replaces `ps` stub)
///
/// Dispatches to the kernel-provided `SYS_INFO_FN` hook.
pub fn cmd_tasks(ctx: &mut CommandContext) -> bool {
    crate::sys_hooks::call_sys_info_hook(ctx, "tasks");
    true
}

/// `windows` — list all windows on the desktop
///
/// Dispatches to the kernel-provided `SYS_INFO_FN` hook.
pub fn cmd_windows(ctx: &mut CommandContext) -> bool {
    crate::sys_hooks::call_sys_info_hook(ctx, "windows");
    true
}

/// `dmesg` — display kernel message buffer
pub fn cmd_dmesg(ctx: &mut CommandContext) -> bool {
    crate::sys_hooks::call_sys_info_hook(ctx, "dmesg");
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
    ctx.terminal.write_str("Fullerene 0.3.0\n");
    ctx.terminal.write_str("Built: 2026-06-06\n");
    ctx.terminal
        .write_str("Components: Lattice, Nozzle, Solvent, ChronoLine, Resonance\n");
    true
}

/// `reboot` — reboot the system
///
/// Dispatches to the kernel-provided system control hook.
pub fn cmd_reboot(ctx: &mut CommandContext) -> bool {
    ctx.terminal.write_str("Rebooting...\n");
    crate::sys_hooks::call_sys_control_hook("reboot");
    true
}

/// `shutdown` — shutdown the system
///
/// Dispatches to the kernel-provided system control hook.
pub fn cmd_shutdown(ctx: &mut CommandContext) -> bool {
    ctx.terminal.write_str("Shutting down...\n");
    crate::sys_hooks::call_sys_control_hook("shutdown");
    true
}

/// `calc` — simple arithmetic calculator
pub fn cmd_calc(ctx: &mut CommandContext) -> bool {
    if ctx.args.len() < 2 {
        ctx.terminal.write_str("Usage: calc <expression>\n");
        ctx.terminal.write_str("Example: calc (2+3)*4\n");
        return true;
    }
    // Join all args into one expression string
    let mut expr = alloc::string::String::new();
    for (i, arg) in ctx.args.iter().enumerate() {
        if i == 0 {
            continue;
        }
        expr.push_str(arg);
    }
    crate::sys_hooks::call_sys_info_hook(ctx, "calc");
    true
}

/// `run` — launch an external application
pub fn cmd_run(ctx: &mut CommandContext) -> bool {
    if ctx.args.len() < 2 {
        ctx.terminal.write_str("Usage: run <app_name>\n");
        crate::sys_hooks::call_sys_info_hook(ctx, "run");
        return true;
    }
    crate::sys_hooks::call_sys_info_hook(ctx, "run");
    true
}

/// `taskmon` — detailed task/process monitor
pub fn cmd_taskmon(ctx: &mut CommandContext) -> bool {
    crate::sys_hooks::call_sys_info_hook(ctx, "taskmon");
    true
}

/// `devices` — list registered hardware devices
pub fn cmd_devices(ctx: &mut CommandContext) -> bool {
    crate::sys_hooks::call_sys_info_hook(ctx, "devices");
    true
}

/// `theme` — show or change the desktop theme
pub fn cmd_theme(ctx: &mut CommandContext) -> bool {
    crate::sys_hooks::call_sys_info_hook(ctx, "theme");
    true
}

/// `wallpaper` — show or change the desktop wallpaper
pub fn cmd_wallpaper(ctx: &mut CommandContext) -> bool {
    crate::sys_hooks::call_sys_info_hook(ctx, "wallpaper");
    true
}

/// `badapple` — play Bad Apple!! on PC speaker with framebuffer animation
pub fn cmd_badapple(ctx: &mut CommandContext) -> bool {
    ctx.terminal.write_str("Bad Apple!! playing... (press any key to stop)\n");
    crate::sys_hooks::call_sys_info_hook(ctx, "badapple");
    true
}
