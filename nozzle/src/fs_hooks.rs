//! Filesystem hooks for Nozzle shell commands.
//!
//! Nozzle has no direct knowledge of the kernel's VFS.  These hooks
//! allow the kernel to register callbacks which the `ls`, `cat`,
//! `pwd`, `cd`, `tree`, `find`, `cp`, `mv`, and `write` commands
//! call into.

use crate::exec::CommandContext;
use spin::Mutex;

/// Callback for listing directories.
pub static FS_LIST_FN: Mutex<Option<fn(&mut CommandContext)>> = Mutex::new(None);

/// Callback for reading file contents.
pub static FS_READ_FN: Mutex<Option<fn(&mut CommandContext, &str)>> = Mutex::new(None);

/// Callback for printing the working directory.
pub static FS_PWD_FN: Mutex<Option<fn(&mut CommandContext)>> = Mutex::new(None);

/// Callback for changing the working directory.
pub static FS_CD_FN: Mutex<Option<fn(&mut CommandContext, &str)>> = Mutex::new(None);

/// Callback for directory tree listing.
pub static FS_TREE_FN: Mutex<Option<fn(&mut CommandContext, &str)>> = Mutex::new(None);

/// Callback for file search (find).
pub static FS_FIND_FN: Mutex<Option<fn(&mut CommandContext, &str, &str)>> = Mutex::new(None);

/// Callback for copying a file.
pub static FS_CP_FN: Mutex<Option<fn(&mut CommandContext, &str, &str)>> = Mutex::new(None);

/// Callback for moving a file.
pub static FS_MV_FN: Mutex<Option<fn(&mut CommandContext, &str, &str)>> = Mutex::new(None);

/// Callback for writing to a file.
pub static FS_WRITE_FN: Mutex<Option<fn(&mut CommandContext, &str, &str)>> = Mutex::new(None);

// ── Setters ────────────────────────────────────────────────────

pub fn set_fs_list_fn(f: fn(&mut CommandContext)) {
    *FS_LIST_FN.lock() = Some(f);
}

pub fn set_fs_read_fn(f: fn(&mut CommandContext, &str)) {
    *FS_READ_FN.lock() = Some(f);
}

pub fn set_fs_pwd_fn(f: fn(&mut CommandContext)) {
    *FS_PWD_FN.lock() = Some(f);
}

pub fn set_fs_cd_fn(f: fn(&mut CommandContext, &str)) {
    *FS_CD_FN.lock() = Some(f);
}

pub fn set_fs_tree_fn(f: fn(&mut CommandContext, &str)) {
    *FS_TREE_FN.lock() = Some(f);
}

pub fn set_fs_find_fn(f: fn(&mut CommandContext, &str, &str)) {
    *FS_FIND_FN.lock() = Some(f);
}

pub fn set_fs_cp_fn(f: fn(&mut CommandContext, &str, &str)) {
    *FS_CP_FN.lock() = Some(f);
}

pub fn set_fs_mv_fn(f: fn(&mut CommandContext, &str, &str)) {
    *FS_MV_FN.lock() = Some(f);
}

pub fn set_fs_write_fn(f: fn(&mut CommandContext, &str, &str)) {
    *FS_WRITE_FN.lock() = Some(f);
}

// ── Dispatchers ────────────────────────────────────────────────

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

pub fn change_directory(ctx: &mut CommandContext, path: &str) {
    if let Some(f) = *FS_CD_FN.lock() {
        f(ctx, path);
    } else {
        ctx.terminal.write_str("cd: no filesystem\n");
    }
}

pub fn tree_directory(ctx: &mut CommandContext, path: &str) {
    if let Some(f) = *FS_TREE_FN.lock() {
        f(ctx, path);
    } else {
        ctx.terminal.write_str("tree: no filesystem\n");
    }
}

pub fn find_files(ctx: &mut CommandContext, path: &str, pattern: &str) {
    if let Some(f) = *FS_FIND_FN.lock() {
        f(ctx, path, pattern);
    } else {
        ctx.terminal.write_str("find: no filesystem\n");
    }
}

pub fn copy_file(ctx: &mut CommandContext, src: &str, dst: &str) {
    if let Some(f) = *FS_CP_FN.lock() {
        f(ctx, src, dst);
    } else {
        ctx.terminal.write_str("cp: no filesystem\n");
    }
}

pub fn move_file(ctx: &mut CommandContext, src: &str, dst: &str) {
    if let Some(f) = *FS_MV_FN.lock() {
        f(ctx, src, dst);
    } else {
        ctx.terminal.write_str("mv: no filesystem\n");
    }
}

pub fn write_file(ctx: &mut CommandContext, path: &str, content: &str) {
    if let Some(f) = *FS_WRITE_FN.lock() {
        f(ctx, path, content);
    } else {
        ctx.terminal.write_str("write: no filesystem\n");
    }
}