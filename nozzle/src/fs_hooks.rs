//! Filesystem hooks for Nozzle shell commands.
//!
//! Nozzle has no direct knowledge of the kernel's VFS.  These hooks
//! allow the kernel to register callbacks which the `ls`, `cat`,
//! `pwd`, `cd`, `tree`, `find`, `cp`, `mv`, and `write` commands
//! call into.
//!
//! All function pointers are bundled into a single [`FsHooks`] value which is
//! constructor-injected into a shell session.

use carrier::exec::CommandContext;

/// Aggregated filesystem hooks for shell built‑in commands.
///
/// Construct this value at the integration boundary and pass it through
/// [`crate::ShellServices`] to the shell session.
#[derive(Clone, Copy)]
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
}

// ── Dispatchers ────────────────────────────────────────────────
// Each function reads the immutable service table injected into the shell.

macro_rules! fs_dispatch {
    ($name:ident, $field:ident, $err:expr) => {
        pub fn $name(ctx: &mut CommandContext) {
            if let Some(f) = crate::services(ctx).and_then(|services| services.fs.$field) {
                f(ctx);
            } else {
                ctx.terminal.write_str($err);
            }
        }
    };
    ($name:ident, $field:ident, $err:expr, $arg:ident: &str) => {
        pub fn $name(ctx: &mut CommandContext, $arg: &str) {
            if let Some(f) = crate::services(ctx).and_then(|services| services.fs.$field) {
                f(ctx, $arg);
            } else {
                ctx.terminal.write_str($err);
            }
        }
    };
    ($name:ident, $field:ident, $err:expr, $a:ident: &str, $b:ident: &str) => {
        pub fn $name(ctx: &mut CommandContext, $a: &str, $b: &str) {
            if let Some(f) = crate::services(ctx).and_then(|services| services.fs.$field) {
                f(ctx, $a, $b);
            } else {
                ctx.terminal.write_str($err);
            }
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
    if let Some(f) = crate::services(ctx).and_then(|services| services.fs.read) {
        f(ctx, path);
    } else {
        ctx.terminal.write_str("(no filesystem mounted: ");
        ctx.terminal.write_str(path);
        ctx.terminal.write_str(")\n");
    }
}
