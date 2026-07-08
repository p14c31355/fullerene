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

macro_rules! fs_dispatch {
    ($name:ident, $field:ident, $err:expr) => {
        pub fn $name(ctx: &mut CommandContext) {
            let hooks = FS_HOOKS.lock();
            if let Some(f) = hooks.$field { drop(hooks); f(ctx); }
            else { drop(hooks); ctx.terminal.write_str($err); }
        }
    };
    ($name:ident, $field:ident, $err:expr, $arg:ident: &str) => {
        pub fn $name(ctx: &mut CommandContext, $arg: &str) {
            let hooks = FS_HOOKS.lock();
            if let Some(f) = hooks.$field { drop(hooks); f(ctx, $arg); }
            else { drop(hooks); ctx.terminal.write_str($err); }
        }
    };
    ($name:ident, $field:ident, $err:expr, $a:ident: &str, $b:ident: &str) => {
        pub fn $name(ctx: &mut CommandContext, $a: &str, $b: &str) {
            let hooks = FS_HOOKS.lock();
            if let Some(f) = hooks.$field { drop(hooks); f(ctx, $a, $b); }
            else { drop(hooks); ctx.terminal.write_str($err); }
        }
    };
}

fs_dispatch!(list_directory, list, "(no filesystem mounted)\n");
fs_dispatch!(print_working_directory, pwd, "/\n");
fs_dispatch!(change_directory, cd, "cd: no filesystem\n", path: &str);
fs_dispatch!(tree_directory, tree, "tree: no filesystem\n", path: &str);
fs_dispatch!(find_files, find, "find: no filesystem\n", path: &str, pattern: &str);
fs_dispatch!(copy_file, cp, "cp: no filesystem\n", src: &str, dst: &str);
fs_dispatch!(move_file, mv, "mv: no filesystem\n", src: &str, dst: &str);
fs_dispatch!(write_file, write, "write: no filesystem\n", path: &str, content: &str);
fs_dispatch!(remove_file, rm, "rm: no filesystem\n", path: &str);
fs_dispatch!(make_directory, mkdir, "mkdir: no filesystem\n", path: &str);
fs_dispatch!(touch_file, touch, "touch: no filesystem\n", path: &str);
fs_dispatch!(disk_usage, df, "df: no filesystem\n");

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
