//! System hooks for Nozzle shell commands.
//!
//! These hooks let the kernel register callbacks for system information
//! commands (`mem`, `tasks`, `windows`, `dmesg`) and system control
//! commands (`reboot`, `shutdown`).
//!
//! Hooks are bundled into a single immutable [`SysHooks`] value and injected
//! into each shell session.

use carrier::exec::CommandContext;

/// Aggregated system hooks.
#[derive(Clone, Copy)]
pub struct SysHooks {
    pub info: Option<fn(&mut CommandContext, &str)>,
    pub ctl: Option<fn(&str)>,
}

impl SysHooks {
    pub const fn none() -> Self {
        Self {
            info: None,
            ctl: None,
        }
    }
}

pub fn call_sys_info_hook(ctx: &mut CommandContext, cmd: &str) {
    if let Some(f) = crate::services(ctx).and_then(|services| services.sys.info) {
        f(ctx, cmd);
    } else {
        ctx.terminal
            .write_str("(not available from this context)\n");
    }
}

pub fn call_sys_control_hook(ctx: &CommandContext, cmd: &str) {
    if let Some(f) = crate::services(ctx).and_then(|services| services.sys.ctl) {
        f(cmd);
    }
}

pub fn call_mount_hook(ctx: &mut CommandContext) {
    if let Some(f) = crate::services(ctx).and_then(|services| services.mount) {
        f(ctx);
    } else {
        ctx.terminal.write_str("mount: service not available\n");
    }
}
