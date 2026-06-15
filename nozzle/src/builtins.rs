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

/// `pci` — list PCI devices
///
/// Dispatches to the kernel-provided `SYS_INFO_FN` hook.
pub fn cmd_pci(ctx: &mut CommandContext) -> bool {
    crate::sys_hooks::call_sys_info_hook(ctx, "pci");
    true
}

/// `calc` — simple arithmetic calculator
pub fn cmd_calc(ctx: &mut CommandContext) -> bool {
    if ctx.args.len() < 2 {
        ctx.terminal.write_str("Usage: calc <expression>\n");
        ctx.terminal.write_str("Example: calc (2+3)*4\n");
        return true;
    }
    // Join all args into one expression string (unused for now as sys_info_hook handles usage)
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
    if ctx.args.len() >= 2 {
        // Route to sys control hook for actual theme change
        let cmd = alloc::format!("theme {}", ctx.args[1]);
        crate::sys_hooks::call_sys_control_hook(&cmd);
        // Force desktop redraw via info hook
        crate::sys_hooks::call_sys_info_hook(ctx, "theme");
        return true;
    }
    crate::sys_hooks::call_sys_info_hook(ctx, "theme");
    true
}

/// `wallpaper` — show or change the desktop wallpaper
pub fn cmd_wallpaper(ctx: &mut CommandContext) -> bool {
    if ctx.args.len() >= 2 {
        let cmd = alloc::format!("wallpaper {}", ctx.args[1]);
        crate::sys_hooks::call_sys_control_hook(&cmd);
        crate::sys_hooks::call_sys_info_hook(ctx, "wallpaper");
        return true;
    }
    crate::sys_hooks::call_sys_info_hook(ctx, "wallpaper");
    true
}

/// `badapple` — play Bad Apple!! on PC speaker with framebuffer animation
pub fn cmd_badapple(ctx: &mut CommandContext) -> bool {
    ctx.terminal
        .write_str("Bad Apple!! playing... (press any key to stop)\n");
    crate::sys_hooks::call_sys_info_hook(ctx, "badapple");
    true
}

/// `cd` — change the current working directory
pub fn cmd_cd(ctx: &mut CommandContext) -> bool {
    if ctx.args.len() < 2 {
        ctx.terminal.write_str("Usage: cd <directory>\n");
        return true;
    }
    crate::fs_hooks::change_directory(ctx, ctx.args[1]);
    true
}

/// `tree` — display a directory tree
pub fn cmd_tree(ctx: &mut CommandContext) -> bool {
    let path = if ctx.args.len() > 1 { &ctx.args[1] } else { "." };
    crate::fs_hooks::tree_directory(ctx, path);
    true
}

/// `find` — search for files matching a pattern
pub fn cmd_find(ctx: &mut CommandContext) -> bool {
    if ctx.args.len() < 3 {
        ctx.terminal
            .write_str("Usage: find <directory> <pattern>\n");
        return true;
    }
    crate::fs_hooks::find_files(ctx, ctx.args[1], ctx.args[2]);
    true
}

/// `cp` — copy a file
pub fn cmd_cp(ctx: &mut CommandContext) -> bool {
    if ctx.args.len() < 3 {
        ctx.terminal.write_str("Usage: cp <source> <destination>\n");
        return true;
    }
    crate::fs_hooks::copy_file(ctx, ctx.args[1], ctx.args[2]);
    true
}

/// `mv` — move a file
pub fn cmd_mv(ctx: &mut CommandContext) -> bool {
    if ctx.args.len() < 3 {
        ctx.terminal.write_str("Usage: mv <source> <destination>\n");
        return true;
    }
    crate::fs_hooks::move_file(ctx, ctx.args[1], ctx.args[2]);
    true
}

/// `write` — write content to a file
pub fn cmd_write(ctx: &mut CommandContext) -> bool {
    if ctx.args.len() < 3 {
        ctx.terminal.write_str("Usage: write <path> <content>\n");
        return true;
    }
    crate::fs_hooks::write_file(ctx, ctx.args[1], ctx.args[2]);
    true
}

/// `rm` — remove a file or directory
pub fn cmd_rm(ctx: &mut CommandContext) -> bool {
    if ctx.args.len() < 2 {
        ctx.terminal.write_str("Usage: rm <path>\n");
        return true;
    }
    for arg in &ctx.args[1..] {
        crate::fs_hooks::remove_file(ctx, arg);
    }
    true
}

/// `mkdir` — create a directory
pub fn cmd_mkdir(ctx: &mut CommandContext) -> bool {
    if ctx.args.len() < 2 {
        ctx.terminal.write_str("Usage: mkdir <path>\n");
        return true;
    }
    for arg in &ctx.args[1..] {
        crate::fs_hooks::make_directory(ctx, arg);
    }
    true
}

/// `touch` — create an empty file or update timestamp
pub fn cmd_touch(ctx: &mut CommandContext) -> bool {
    if ctx.args.len() < 2 {
        ctx.terminal.write_str("Usage: touch <path>\n");
        return true;
    }
    for arg in &ctx.args[1..] {
        crate::fs_hooks::touch_file(ctx, arg);
    }
    true
}

/// `df` — show disk usage
pub fn cmd_df(ctx: &mut CommandContext) -> bool {
    crate::fs_hooks::disk_usage(ctx);
    true
}

/// `date` — show current date and time
pub fn cmd_date(ctx: &mut CommandContext) -> bool {
    crate::sys_hooks::call_sys_info_hook(ctx, "date");
    true
}

/// `uptime` — show system uptime
pub fn cmd_uptime(ctx: &mut CommandContext) -> bool {
    crate::sys_hooks::call_sys_info_hook(ctx, "uptime");
    true
}

/// `whoami` — print current user name
pub fn cmd_whoami(ctx: &mut CommandContext) -> bool {
    ctx.terminal.write_str("fullerene\n");
    true
}

/// `history` — show command history
pub fn cmd_history(ctx: &mut CommandContext) -> bool {
    let entries = crate::line_editor::get_history();
    if entries.is_empty() {
        ctx.terminal.write_str("(no history)\n");
    } else {
        for (num, entry) in entries.iter().rev().enumerate() {
            let line = alloc::format!("{}  {}\n", num + 1, entry);
            ctx.terminal.write_str(&line);
        }
    }
    true
}

/// `sleep` — pause for a number of seconds
pub fn cmd_sleep(ctx: &mut CommandContext) -> bool {
    if ctx.args.len() < 2 {
        ctx.terminal.write_str("Usage: sleep <seconds>\n");
        return true;
    }
    crate::sys_hooks::call_sys_info_hook(ctx, "sleep");
    true
}

/// `grep` — search for a pattern in input (stdin or files)
pub fn cmd_grep(ctx: &mut CommandContext) -> bool {
    if ctx.args.len() < 2 {
        ctx.terminal.write_str("Usage: grep <pattern> [file...]\n");
        ctx.terminal.write_str("       command | grep <pattern>\n");
        return true;
    }
    let pattern = ctx.args[1];
    // If stdin was provided (from a pipe), search through it.
    if let Some(stdin) = ctx.terminal.take_stdin() {
        for line in stdin.lines() {
            if line.contains(pattern) {
                ctx.terminal.write_str(line);
                ctx.terminal.write_str("\n");
            }
        }
        return true;
    }
    // Otherwise, search files provided as arguments.
    if ctx.args.len() < 3 {
        ctx.terminal
            .write_str("grep: no input (pipe data or specify files)\n");
        return true;
    }
    // Use a simple sys_info dispatch for file-based grep
    // The kernel will process all files in ctx.args[2..]
    crate::sys_hooks::call_sys_info_hook(ctx, "grep");
    true
}

/// `sort` — sort lines of text
pub fn cmd_sort(ctx: &mut CommandContext) -> bool {
    let reverse = ctx.args.iter().any(|a| *a == "-r");
    // Try reading from stdin first (pipe input).
    if let Some(stdin) = ctx.terminal.take_stdin() {
        let mut lines: alloc::vec::Vec<&str> = stdin.lines().collect();
        lines.sort();
        if reverse {
            lines.reverse();
        }
        for line in lines {
            ctx.terminal.write_str(line);
            ctx.terminal.write_str("\n");
        }
        return true;
    }
    // If no stdin, try reading from a file.
    if ctx.args.len() > 1 {
        crate::sys_hooks::call_sys_info_hook(ctx, "sort");
    } else {
        ctx.terminal.write_str("Usage: sort [-r] [file]\n");
        ctx.terminal.write_str("       command | sort [-r]\n");
    }
    true
}

/// `wc` — count lines, words, and bytes
pub fn cmd_wc(ctx: &mut CommandContext) -> bool {
    // Read from stdin (pipe input) or files.
    if let Some(stdin) = ctx.terminal.take_stdin() {
        let lines = stdin.lines().count();
        let words = stdin.split_whitespace().count();
        let bytes = stdin.len();
        let out = alloc::format!("{} {} {} (stdin)\n", lines, words, bytes);
        ctx.terminal.write_str(&out);
        return true;
    }
    // From files
    if ctx.args.len() > 1 {
        crate::sys_hooks::call_sys_info_hook(ctx, "wc");
    } else {
        ctx.terminal.write_str("Usage: wc [file]\n");
        ctx.terminal.write_str("       command | wc\n");
    }
    true
}

/// `app` — package manager (install / remove / list)
pub fn cmd_app(ctx: &mut CommandContext) -> bool {
    if ctx.args.len() < 2 {
        ctx.terminal
            .write_str("Usage: app <install|remove|list> [name] [description]\n");
        return true;
    }
    let sub = ctx.args[1];
    match sub {
        "list" => crate::sys_hooks::call_sys_info_hook(ctx, "app_list"),
        "install" if ctx.args.len() >= 4 => {
            let name = ctx.args[2];
            let desc = ctx.args[3];
            // Use sys_control hook: "app_install <name> <desc>"
            let cmd = alloc::format!("app_install {} {}", name, desc);
            crate::sys_hooks::call_sys_control_hook(&cmd);
        }
        "install" => {
            ctx.terminal
                .write_str("Usage: app install <name> <description>\n");
        }
        "remove" if ctx.args.len() >= 3 => {
            let name = ctx.args[2];
            let cmd = alloc::format!("app_remove {}", name);
            crate::sys_hooks::call_sys_control_hook(&cmd);
        }
        "remove" => {
            ctx.terminal.write_str("Usage: app remove <name>\n");
        }
        _ => {
            let msg = alloc::format!("app: unknown subcommand '{}'\n", sub);
            ctx.terminal.write_str(&msg);
        }
    }
    true
}
