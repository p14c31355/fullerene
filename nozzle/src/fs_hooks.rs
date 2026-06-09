//! Filesystem hooks for Nozzle shell commands.
//!
//! Nozzle has no direct knowledge of the kernel's VFS.  These hooks
//! allow the kernel to register callbacks which the `ls`, `cat`, and
//! `pwd` commands call into.

use crate::exec::CommandContext;
use alloc::string::String;
use spin::Mutex;

/// Callback for listing directories.
pub static FS_LIST_FN: Mutex<Option<fn(&mut CommandContext)>> = Mutex::new(None);

/// Callback for reading file contents.
pub static FS_READ_FN: Mutex<Option<fn(&mut CommandContext, &str)>> = Mutex::new(None);

/// Callback for printing the working directory.
pub static FS_PWD_FN: Mutex<Option<fn(&mut CommandContext)>> = Mutex::new(None);

pub fn set_fs_list_fn(f: fn(&mut CommandContext)) {
    *FS_LIST_FN.lock() = Some(f);
}

pub fn set_fs_read_fn(f: fn(&mut CommandContext, &str)) {
    *FS_READ_FN.lock() = Some(f);
}

pub fn set_fs_pwd_fn(f: fn(&mut CommandContext)) {
    *FS_PWD_FN.lock() = Some(f);
}

pub fn list_directory(ctx: &mut CommandContext) {
    if let Some(f) = *FS_LIST_FN.lock() {
        f(ctx);
    } else {
        ctx.terminal.write_str("(no filesystem mounted)\n");
    }
}

pub fn read_file(ctx: &mut CommandContext, path: &str) {
    if let Some(f) = *FS_READ_FN.lock() {
        f(ctx, path);
    } else {
        ctx.terminal.write_str("(no filesystem mounted: ");
        ctx.terminal.write_str(path);
        ctx.terminal.write_str(")\n");
    }
}

pub fn print_working_directory(ctx: &mut CommandContext) {
    if let Some(f) = *FS_PWD_FN.lock() {
        f(ctx);
    } else {
        ctx.terminal.write_str("/\n");
    }
}
