//! Filesystem hooks for Nozzle shell commands.
//!
//! Nozzle has no direct knowledge of the kernel's VFS.  These hooks
//! allow the kernel to register callbacks which the `ls`, `cat`,
//! `pwd`, `cd`, `tree`, `find`, `cp`, `mv`, and `write` commands
//! call into.
//!
//! All 13 function pointers are bundled into a single [`FsHooks`] struct
//! stored under one `Mutex`, eliminating repetitive per‑hook statics.

use carrier::exec::CommandContext;
use spin::Mutex;

/// Aggregated filesystem hooks for shell built‑in commands.
///
/// Set all hooks at once via [`FsHooks::install()`] or by assigning
/// to [`FS_HOOKS`] directly.
pub struct FsHooks {
    pub list: Option<fn(&mut CommandContext)>,
    pub read: Option<fn(&mut CommandContext, &str)>,
    pub pwd: Option<fn(&mut CommandContext)>,
    pub cd: Option<fn(&mut CommandContext, &str)>,
    pub tree: Option<fn(&mut CommandContext, &str)>,
    pub find: Option<fn(&mut CommandContext, &str, &str)>,
    pub cp: Option<fn(&mut CommandContext, &str, &str)>,
    pub mv: Option<fn(&mut CommandContext, &str, &str)>,
    pub write: Option<fn(&mut CommandContext, &str, &str)>,
    pub rm: Option<fn(&mut CommandContext, &str)>,
    pub mkdir: Option<fn(&mut CommandContext, &str)>,
    pub touch: Option<fn(&mut CommandContext, &str)>,
    pub df: Option<fn(&mut CommandContext)>,
}

impl FsHooks {
    /// Build a no‑op set of hooks (every field is `None`).
    pub const fn none() -> Self {
        Self {
            list: None,
            read: None,
            pwd: None,
            cd: None,
            tree: None,
            find: None,
            cp: None,
            mv: None,
            write: None,
            rm: None,
            mkdir: None,
            touch: None,
            df: None,
        }
    }

    /// Atomically install this set of hooks into the global [`FS_HOOKS`].
    pub fn install(self) {
        *FS_HOOKS.lock() = self;
    }
}

/// Global filesystem‑hooks bag.
///
/// Access is always via the dispatcher functions below; the caller
/// never locks `FS_HOOKS` directly.
pub static FS_HOOKS: Mutex<FsHooks> = Mutex::new(FsHooks::none());

// ── Dispatchers ────────────────────────────────────────────────
// Each function reads the single `FS_HOOKS` lock once and forwards.

pub fn list_directory(ctx: &mut CommandContext) {
    let hooks = FS_HOOKS.lock();
    if let Some(f) = hooks.list {
        drop(hooks);
        f(ctx);
    } else {
        drop(hooks);
        ctx.terminal.write_str("(no filesystem mounted)\n");
    }
}

pub fn read_file(ctx: &mut CommandContext, path: &str) {
    let hooks = FS_HOOKS.lock();
    if let Some(f) = hooks.read {
        drop(hooks);
        f(ctx, path);
    } else {
        drop(hooks);
        ctx.terminal.write_str("(no filesystem mounted: ");
        ctx.terminal.write_str(path);
        ctx.terminal.write_str(")\n");
    }
}

pub fn print_working_directory(ctx: &mut CommandContext) {
    let hooks = FS_HOOKS.lock();
    if let Some(f) = hooks.pwd {
        drop(hooks);
        f(ctx);
    } else {
        drop(hooks);
        ctx.terminal.write_str("/\n");
    }
}

pub fn change_directory(ctx: &mut CommandContext, path: &str) {
    let hooks = FS_HOOKS.lock();
    if let Some(f) = hooks.cd {
        drop(hooks);
        f(ctx, path);
    } else {
        drop(hooks);
        ctx.terminal.write_str("cd: no filesystem\n");
    }
}

pub fn tree_directory(ctx: &mut CommandContext, path: &str) {
    let hooks = FS_HOOKS.lock();
    if let Some(f) = hooks.tree {
        drop(hooks);
        f(ctx, path);
    } else {
        drop(hooks);
        ctx.terminal.write_str("tree: no filesystem\n");
    }
}

pub fn find_files(ctx: &mut CommandContext, path: &str, pattern: &str) {
    let hooks = FS_HOOKS.lock();
    if let Some(f) = hooks.find {
        drop(hooks);
        f(ctx, path, pattern);
    } else {
        drop(hooks);
        ctx.terminal.write_str("find: no filesystem\n");
    }
}

pub fn copy_file(ctx: &mut CommandContext, src: &str, dst: &str) {
    let hooks = FS_HOOKS.lock();
    if let Some(f) = hooks.cp {
        drop(hooks);
        f(ctx, src, dst);
    } else {
        drop(hooks);
        ctx.terminal.write_str("cp: no filesystem\n");
    }
}

pub fn move_file(ctx: &mut CommandContext, src: &str, dst: &str) {
    let hooks = FS_HOOKS.lock();
    if let Some(f) = hooks.mv {
        drop(hooks);
        f(ctx, src, dst);
    } else {
        drop(hooks);
        ctx.terminal.write_str("mv: no filesystem\n");
    }
}

pub fn write_file(ctx: &mut CommandContext, path: &str, content: &str) {
    let hooks = FS_HOOKS.lock();
    if let Some(f) = hooks.write {
        drop(hooks);
        f(ctx, path, content);
    } else {
        drop(hooks);
        ctx.terminal.write_str("write: no filesystem\n");
    }
}

pub fn remove_file(ctx: &mut CommandContext, path: &str) {
    let hooks = FS_HOOKS.lock();
    if let Some(f) = hooks.rm {
        drop(hooks);
        f(ctx, path);
    } else {
        drop(hooks);
        ctx.terminal.write_str("rm: no filesystem\n");
    }
}

pub fn make_directory(ctx: &mut CommandContext, path: &str) {
    let hooks = FS_HOOKS.lock();
    if let Some(f) = hooks.mkdir {
        drop(hooks);
        f(ctx, path);
    } else {
        drop(hooks);
        ctx.terminal.write_str("mkdir: no filesystem\n");
    }
}

pub fn touch_file(ctx: &mut CommandContext, path: &str) {
    let hooks = FS_HOOKS.lock();
    if let Some(f) = hooks.touch {
        drop(hooks);
        f(ctx, path);
    } else {
        drop(hooks);
        ctx.terminal.write_str("touch: no filesystem\n");
    }
}

pub fn disk_usage(ctx: &mut CommandContext) {
    let hooks = FS_HOOKS.lock();
    if let Some(f) = hooks.df {
        drop(hooks);
        f(ctx);
    } else {
        drop(hooks);
        ctx.terminal.write_str("df: no filesystem\n");
    }
}
